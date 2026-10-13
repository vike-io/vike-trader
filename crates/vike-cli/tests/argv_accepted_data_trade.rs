//! The `data` and `trade` planes' command lines, pinned as ACCEPTED (decision 0155).
//!
//! Written against the hand-rolled `Flags` parsers (`cmd/data/**/grammar.rs`, `cmd/data/hist/parse*.rs`,
//! `cmd/trade/**`) and run UNCHANGED against the `clap` ones: every line below is one an operator, a
//! runbook, a skill or a verb's own usage page writes today, and the only question asked is whether
//! the argument reader let it through (`common::accepted` explains the sandbox and why "not exit 2"
//! is that question). Lines the old readers REFUSE on purpose (a retired spelling, a flag answered
//! by name) are pinned as refused so a rewrite keeps refusing them. Dials go to [`DEAD`] (exit 3)
//! and engine routes to [`NO_ENGINE`] (exit 3), never to a live default address or a located engine;
//! the one exception is `data_hist_reads_on_the_default_address` (read-only). The `trade` lines end
//! at exit 1 before any socket opens: the sandbox holds no node key.

mod common;

use common::accepted::{Pin, assert_accepted, pin, pin_code};

/// A loopback port nothing listens on: a dial is refused at once (exit 3).
const DEAD: &str = "127.0.0.1:1";

/// An engine path that does not exist: the spawn fails (exit 3) instead of running a located engine.
const NO_ENGINE: &str = "no-such-engine";

/// A bar spec every verb that takes `VENUE:SYMBOL:INTERVAL` accepts.
const SPEC: &str = "binance:BTCUSDT:1h";

// ─── data hist: the READ half ────────────────────────────────────────────────────────────────────

/// `ls`, `gaps`, `coverage`, `health`, `universe`: the filter verbs, each flag once, in both spellings.
#[test]
fn data_hist_listing_reads() {
    assert_accepted(&[
        pin(&["data", "hist", "ls", "--addr", DEAD], "ls: --addr"),
        pin(&["data", "hist", "ls", "--addr", DEAD, "--kind", "bar"], "ls: --kind"),
        pin(&["data", "hist", "ls", "--addr", DEAD, "--venue", "binance"], "ls: --venue"),
        pin(&["data", "hist", "ls", "--addr", DEAD, "--name", "BTC"], "ls: --name"),
        pin(
            &["data", "hist", "ls", "--addr", DEAD, "--class"],
            "ls: --class is a bare switch here",
        ),
        pin(&["data", "hist", "ls", "--addr", DEAD, "--json"], "ls: --json"),
        pin(&["data", "hist", "ls", "--addr", DEAD, "--format", "json"], "ls: --format json"),
        pin(&["data", "hist", "ls", "--addr", DEAD, "--format", "table"], "ls: --format table"),
        pin(
            &[
                "data", "hist", "ls", "--addr", DEAD, "--kind", "bar", "--venue", "oanda",
                "--name", "EUR", "--class", "--json",
            ],
            "ls: every flag at once",
        ),
        pin(
            &[
                "data",
                "hist",
                "ls",
                "--addr=127.0.0.1:1",
                "--kind=bar",
                "--venue=oanda",
                "--name=EUR",
                "--format=json",
            ],
            "ls: the inline --flag=value spelling of every valued flag",
        ),
        pin(
            &["data", "hist", "ls", "--json", "--venue", "oanda", "--addr", DEAD],
            "ls: flag order is free",
        ),
        pin(
            &["data", "hist", "ls", "--addr", DEAD, "--json", "--format", "json"],
            "ls: --json beside --format json agree, so both are accepted",
        ),
        pin(&["data", "hist", "gaps", "--addr", DEAD], "gaps: --addr"),
        pin(
            &[
                "data", "hist", "gaps", "--addr", DEAD, "--kind", "bar", "--venue", "binance",
                "--name", "BTC", "--json",
            ],
            "gaps: --kind --venue --name --json",
        ),
        pin(
            &[
                "data",
                "hist",
                "gaps",
                "--addr=127.0.0.1:1",
                "--kind=trade",
                "--venue=binance",
                "--name=BTC",
                "--format=json",
            ],
            "gaps: inline spellings",
        ),
        pin(&["data", "hist", "coverage", "--addr", DEAD], "coverage: --addr"),
        pin(
            &[
                "data",
                "hist",
                "coverage",
                "--addr",
                DEAD,
                "--venue",
                "binance",
                "--name",
                "BTC",
                "--partial-only",
                "--json",
            ],
            "coverage: --venue --name --partial-only --json",
        ),
        pin(
            &["data", "hist", "coverage", "--partial-only", "--addr", DEAD],
            "coverage: --partial-only before --addr",
        ),
        pin(
            &[
                "data",
                "hist",
                "coverage",
                "--addr=127.0.0.1:1",
                "--venue=binance",
                "--name=BTC",
                "--format=json",
            ],
            "coverage: inline spellings",
        ),
        pin(&["data", "hist", "health", "--addr", DEAD], "health: --addr"),
        pin(
            &[
                "data", "hist", "health", "--addr", DEAD, "--kind", "trade", "--venue", "binance",
                "--name", "BTC", "--json",
            ],
            "health: --kind --venue --name --json",
        ),
        pin(
            &["data", "hist", "health", "--addr=127.0.0.1:1", "--kind=bar", "--format=json"],
            "health: inline spellings",
        ),
        pin(&["data", "hist", "universe", "--addr", DEAD], "universe: no window at all"),
        pin(
            &[
                "data",
                "hist",
                "universe",
                "--addr",
                DEAD,
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--name",
                "BTC",
                "--from",
                "2024-01-01",
                "--to",
                "2024-06-01",
                "--json",
            ],
            "universe: every flag",
        ),
        pin(
            &["data", "hist", "universe", "--addr", DEAD, "--from", "2024-01-01"],
            "universe: --from stands alone",
        ),
        pin(
            &["data", "hist", "universe", "--addr", DEAD, "--to", "2024-06-01"],
            "universe: --to stands alone",
        ),
        pin(
            &[
                "data",
                "hist",
                "universe",
                "--addr",
                DEAD,
                "--from",
                "1704067200000",
                "--to",
                "1717200000000",
            ],
            "universe: epoch-ms bounds",
        ),
        pin(
            &["data", "hist", "universe", "--addr", DEAD, "--from", "-86400000", "--to", "0"],
            "universe: a NEGATIVE epoch-ms is a value (only a `--` token is a flag)",
        ),
        pin(
            &["data", "hist", "universe", "--addr", DEAD, "--from=-86400000", "--to=0"],
            "universe: a negative epoch-ms in the inline spelling",
        ),
        pin(
            &[
                "data",
                "hist",
                "universe",
                "--addr=127.0.0.1:1",
                "--from=2024-01-01",
                "--to=2024-06-01",
                "--kind=bar",
                "--venue=binance",
                "--name=BTC",
                "--format=json",
            ],
            "universe: inline spellings",
        ),
    ]);
}

/// `get`: the rows verb, with its window, its ceiling and its three renderings.
#[test]
fn data_hist_get() {
    assert_accepted(&[
        pin(
            &["data", "hist", "get", SPEC, "--days", "7", "--limit", "20", "--addr", DEAD],
            "usage.rs / the verb's doc line: --days N --limit N",
        ),
        pin(&["data", "hist", "get", SPEC, "--days", "7", "--addr", DEAD], "get: --days"),
        pin(
            &[
                "data",
                "hist",
                "get",
                SPEC,
                "--from",
                "2026-09-01",
                "--limit",
                "100",
                "--addr",
                DEAD,
            ],
            "surface design 7.0's worked example: a lone --from",
        ),
        pin(
            &["data", "hist", "get", SPEC, "--to", "2026-09-01", "--addr", DEAD],
            "get: a lone --to is a half-open window",
        ),
        pin(
            &[
                "data",
                "hist",
                "get",
                SPEC,
                "--from",
                "2026-09-01",
                "--to",
                "2026-09-02",
                "--addr",
                DEAD,
            ],
            "get: --from and --to",
        ),
        pin(
            &["data", "hist", "get", SPEC, "--from", "-86400000", "--to", "0", "--addr", DEAD],
            "get: a NEGATIVE epoch-ms bound is a value",
        ),
        pin(
            &["data", "hist", "get", SPEC, "--days", "7", "--format", "jsonl", "--addr", DEAD],
            "get: --format jsonl, the pipeline form",
        ),
        pin(
            &["data", "hist", "get", SPEC, "--days", "7", "--format", "json", "--addr", DEAD],
            "get: --format json",
        ),
        pin(
            &["data", "hist", "get", SPEC, "--days", "7", "--format", "table", "--addr", DEAD],
            "get: --format table",
        ),
        pin(&["data", "hist", "get", SPEC, "--days", "7", "--json", "--addr", DEAD], "get: --json"),
        pin(
            &["data", "hist", "get", "--days", "7", "--addr", DEAD, SPEC],
            "get: flags may precede the positional spec",
        ),
        pin(
            &[
                "data",
                "hist",
                "get",
                SPEC,
                "--days=7",
                "--limit=20",
                "--format=jsonl",
                "--addr=127.0.0.1:1",
            ],
            "get: inline spellings",
        ),
        pin(
            &["data", "hist", "get", "binance:BTCUSDT.P:funding", "--days", "30", "--addr", DEAD],
            "get: the funding lane's spec (interval `funding`, symbol with a dot)",
        ),
    ]);
}

/// `gate`: the verb whose product is the exit code; spec shapes, criteria and the repeatable flag.
#[test]
fn data_hist_gate() {
    assert_accepted(&[
        pin(
            &[
                "data",
                "hist",
                "gate",
                SPEC,
                "--require-days",
                "365",
                "--max-gap",
                "1d",
                "--addr",
                DEAD,
            ],
            "gate's own usage example",
        ),
        pin(
            &["data", "hist", "gate", SPEC, "--require-days", "1", "--addr", DEAD],
            "gate: the narrowest gate",
        ),
        pin(
            &[
                "data",
                "hist",
                "gate",
                SPEC,
                "--require-days",
                "30",
                "--max-gap",
                "4h",
                "--addr",
                DEAD,
            ],
            "gate: --max-gap in hours",
        ),
        pin(
            &[
                "data",
                "hist",
                "gate",
                SPEC,
                "--require-days",
                "30",
                "--max-gap",
                "2w",
                "--addr",
                DEAD,
            ],
            "gate: --max-gap in weeks",
        ),
        pin(
            &[
                "data",
                "hist",
                "gate",
                SPEC,
                "--require-days",
                "30",
                "--require-kind",
                "bar",
                "--addr",
                DEAD,
            ],
            "gate: --require-kind bar on a spec with an interval",
        ),
        pin(
            &[
                "data",
                "hist",
                "gate",
                "binance:BTCUSDT",
                "--require-days",
                "30",
                "--require-kind",
                "trade",
                "--require-kind",
                "quote",
                "--addr",
                DEAD,
            ],
            "gate: --require-kind repeated (a SET), on a spec with no interval",
        ),
        pin(
            &[
                "data",
                "hist",
                "gate",
                "binance:BTCUSDT",
                "--require-days",
                "30",
                "--require-kind",
                "trade",
                "--require-kind",
                "trade",
                "--addr",
                DEAD,
            ],
            "gate: a repeated kind collapses",
        ),
        pin(
            &[
                "data",
                "hist",
                "gate",
                "polymarket:@btc-5m",
                "--require-days",
                "1",
                "--require-kind",
                "book",
                "--addr",
                DEAD,
            ],
            "gate: VENUE:@GROUP spec (a positional beginning with @ after the colon)",
        ),
        pin(
            &[
                "data",
                "hist",
                "gate",
                SPEC,
                "--require-days",
                "365",
                "--max-gap",
                "1d",
                "--json",
                "--addr",
                DEAD,
            ],
            "gate: --json",
        ),
        pin(
            &[
                "data",
                "hist",
                "gate",
                SPEC,
                "--require-days",
                "365",
                "--format",
                "json",
                "--addr",
                DEAD,
            ],
            "gate: --format json",
        ),
        pin(
            &["data", "hist", "gate", "--require-days", "7", "--addr", DEAD, SPEC],
            "gate: flags may precede the positional spec",
        ),
        pin(
            &[
                "data",
                "hist",
                "gate",
                SPEC,
                "--require-days=365",
                "--max-gap=1d",
                "--require-kind=bar",
                "--addr=127.0.0.1:1",
            ],
            "gate: inline spellings, --require-kind included",
        ),
    ]);
}

/// `running` and `cancel`: the registry of a datahub's running fetches.
#[test]
fn data_hist_running_and_cancel() {
    assert_accepted(&[
        pin(&["data", "hist", "running", "--addr", DEAD], "docs/ops: running"),
        pin(&["data", "hist", "running", "--addr", DEAD, "--json"], "running: --json"),
        pin(
            &["data", "hist", "running", "--addr", DEAD, "--format", "json"],
            "running: --format json",
        ),
        pin(
            &["data", "hist", "running", "--addr", DEAD, "--format", "table"],
            "running: --format table",
        ),
        pin(&["data", "hist", "running", "--addr=127.0.0.1:1"], "running: inline --addr"),
        pin(
            &["data", "hist", "cancel", "oanda:EUR_USD:5s", "--addr", DEAD],
            "docs/ops: cancel SPEC",
        ),
        pin(
            &["data", "hist", "cancel", "oanda:EUR_USD:5s", "--addr", DEAD, "--json"],
            "cancel: --json",
        ),
        pin(
            &["data", "hist", "cancel", "oanda:EUR_USD:5s", "--addr", DEAD, "--format", "json"],
            "cancel: --format json",
        ),
        pin(
            &["data", "hist", "cancel", "--addr", DEAD, "oanda:EUR_USD:5s"],
            "cancel: --addr before the spec",
        ),
        pin(
            &["data", "hist", "cancel", "oanda:EUR_USD:5s", "--addr=127.0.0.1:1"],
            "cancel: inline --addr",
        ),
    ]);
}

// ─── data hist: the WRITE half ───────────────────────────────────────────────────────────────────

/// `fetch` against a datahub: spec shapes, both window forms, and the flags it keeps.
#[test]
fn data_hist_fetch_a_venue() {
    assert_accepted(&[
        pin(
            &["data", "hist", "fetch", SPEC, "--days", "180", "--addr", DEAD],
            "README / get-market-data skill: --days",
        ),
        pin(
            &[
                "data",
                "hist",
                "fetch",
                "okx:BTC-USDT:1h",
                "--from",
                "2024-01-01",
                "--to",
                "2024-06-01",
                "--addr",
                DEAD,
            ],
            "get-market-data skill: a --from/--to window and a dash inside the symbol",
        ),
        pin(
            &[
                "data",
                "hist",
                "fetch",
                "oanda:EUR_USD:5s",
                "--from",
                "2005-01-03",
                "--to",
                "2026-09-29",
                "--addr",
                DEAD,
            ],
            "get-market-data skill: a window longer than a year (split per calendar year)",
        ),
        pin(
            &[
                "data",
                "hist",
                "fetch",
                SPEC,
                "--from",
                "1704067200000",
                "--to",
                "1706745600000",
                "--addr",
                DEAD,
            ],
            "scripts/publish_starter_data.sh: epoch-ms bounds",
        ),
        pin(
            &["data", "hist", "fetch", "binance:BTCUSDT.P:funding", "--days", "30", "--addr", DEAD],
            "usage.rs: the funding-rate series spec",
        ),
        pin(
            &["data", "hist", "fetch", "dukascopy:EURUSD:7m", "--days", "30", "--addr", DEAD],
            "docs/ops: a derived bar step (7m) is a plain spec",
        ),
        pin(
            &["data", "hist", "fetch", SPEC, "--days", "30", "--addr", DEAD, "--json"],
            "fetch: --json",
        ),
        pin(
            &["data", "hist", "fetch", SPEC, "--days", "30", "--addr", DEAD, "--format", "json"],
            "fetch: --format json",
        ),
        pin(
            &["data", "hist", "fetch", "--days", "30", "--addr", DEAD, SPEC],
            "fetch: flags before the positional",
        ),
        pin(
            &["data", "hist", "fetch", SPEC, "--days=180", "--addr=127.0.0.1:1"],
            "fetch: inline spellings",
        ),
        pin(
            &[
                "data",
                "hist",
                "fetch",
                SPEC,
                "--from=2024-01-01",
                "--to=2024-06-01",
                "--addr=127.0.0.1:1",
            ],
            "fetch: inline window",
        ),
        pin(
            &["data", "hist", "fetch", SPEC, "--days", "30", "--source", "okx", "--addr", DEAD],
            "fetch: --source with any word that is not a named source is read as a VENUE",
        ),
        pin(
            &["data", "hist", "fetch", SPEC, "--days", "30", "--source", "venue", "--addr", DEAD],
            "fetch: --source venue",
        ),
    ]);
}

/// `fetch --source starter|demo`: the engine routes, which take no spec and no window.
#[test]
fn data_hist_fetch_starter_and_demo() {
    assert_accepted(&[
        pin(
            &[
                "data", "hist", "fetch", "--source", "demo", "--engine", NO_ENGINE, "--store",
                "store",
            ],
            "scripts/cli_mcp_smoke.sh: --source demo --engine E --store S",
        ),
        pin(
            &[
                "data", "hist", "fetch", "--source", "starter", "--engine", NO_ENGINE, "--store",
                "store",
            ],
            "README: --source starter",
        ),
        pin(
            &["data", "hist", "fetch", "--source", "demo", "--engine", NO_ENGINE],
            "--source demo with no --store",
        ),
        pin(
            &["data", "hist", "fetch", "--source", "starter", "--engine", NO_ENGINE],
            "--source starter with no --store",
        ),
        pin(
            &["data", "hist", "fetch", "--source=demo", "--engine=no-such-engine", "--store=store"],
            "inline spellings",
        ),
        pin(
            &[
                "data", "hist", "fetch", "--source", "demo", "--engine", NO_ENGINE, "--store",
                "store", "--json",
            ],
            "--json on the engine route",
        ),
        pin(
            &[
                "data", "hist", "fetch", "--source", "demo", "--engine", NO_ENGINE, "--store",
                "store", "--format", "json",
            ],
            "--format json on the engine route",
        ),
        pin(
            &[
                "data", "hist", "fetch", "--store", "store", "--engine", NO_ENGINE, "--source",
                "starter",
            ],
            "flag order is free",
        ),
    ]);
}

/// `import`: two positionals, the day bounds, the bar list and the plan-then-confirm pair.
#[test]
fn data_hist_import() {
    assert_accepted(&[
        pin(
            &["data", "hist", "import", "dukascopy-bi5", "EURUSD", "--dry-run", "--addr", DEAD],
            "docs/ops/dukascopy-archive-import.md",
        ),
        pin(
            &[
                "data",
                "hist",
                "import",
                "dukascopy-bi5",
                "EURUSD",
                "--dry-run",
                "--verify",
                "--addr",
                DEAD,
            ],
            "docs/ops/dukascopy-archive-import.md: --dry-run --verify",
        ),
        pin(
            &[
                "data",
                "hist",
                "import",
                "dukascopy-bi5",
                "EURUSD",
                "--from",
                "2024-01-01",
                "--to",
                "2024-01-31",
                "--yes",
                "--addr",
                DEAD,
            ],
            "get-market-data skill: a day window with --yes",
        ),
        pin(
            &[
                "data",
                "hist",
                "import",
                "dukascopy-bi5",
                "EURUSD",
                "--dry-run",
                "--bars",
                "1m,5m",
                "--addr",
                DEAD,
            ],
            "import: --bars is a comma list",
        ),
        pin(
            &[
                "data",
                "hist",
                "import",
                "dukascopy-bi5",
                "EURUSD",
                "--dry-run",
                "--bars",
                "none",
                "--addr",
                DEAD,
            ],
            "import: --bars none stores the ticks alone",
        ),
        pin(
            &[
                "data",
                "hist",
                "import",
                "dukascopy-bi5",
                "EURUSD",
                "--dry-run",
                "--from",
                "1704067200000",
                "--to",
                "1704067200000",
                "--addr",
                DEAD,
            ],
            "import: epoch-ms of a UTC midnight is a day",
        ),
        pin(
            &[
                "data",
                "hist",
                "import",
                "dukascopy-bi5",
                "EURUSD",
                "--dry-run",
                "--json",
                "--addr",
                DEAD,
            ],
            "import: --json",
        ),
        pin(
            &[
                "data",
                "hist",
                "import",
                "dukascopy-bi5",
                "EURUSD",
                "--dry-run",
                "--format",
                "json",
                "--addr",
                DEAD,
            ],
            "import: --format json",
        ),
        pin(
            &["data", "hist", "import", "--dry-run", "--addr", DEAD, "dukascopy-bi5", "EURUSD"],
            "import: flags before the two positionals",
        ),
        pin(
            &["data", "hist", "import", "dukascopy-bi5", "--dry-run", "EURUSD", "--addr", DEAD],
            "import: a flag BETWEEN the two positionals",
        ),
        pin(
            &[
                "data",
                "hist",
                "import",
                "dukascopy-bi5",
                "EURUSD",
                "--from=2024-01-01",
                "--to=2024-01-31",
                "--bars=1m,1h",
                "--dry-run",
                "--addr=127.0.0.1:1",
            ],
            "import: inline spellings",
        ),
    ]);
}

/// `export`: the ENGINE route (Parquet) and the REMOTE route (`--addr`, rows).
#[test]
fn data_hist_export() {
    assert_accepted(&[
        // The engine route.
        pin(
            &["data", "hist", "export", SPEC, "--out", "btc-1h.parquet", "--engine", NO_ENGINE],
            "README / get-market-data skill: engine route",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out",
                "btc-1h.parquet",
                "--from",
                "2024-01-01T00",
                "--engine",
                NO_ENGINE,
            ],
            "get-market-data skill: an HOUR label, forwarded to the engine unparsed",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out",
                "x.parquet",
                "--to",
                "2024-01-01T00",
                "--engine",
                NO_ENGINE,
            ],
            "export (engine): --to stands alone",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out",
                "x.parquet",
                "--from",
                "1704067200000",
                "--to",
                "1706745600000",
                "--engine",
                NO_ENGINE,
            ],
            "export (engine): epoch-ms bounds",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out",
                "x.parquet",
                "--format",
                "parquet",
                "--engine",
                NO_ENGINE,
            ],
            "export (engine): --format parquet names what the route writes",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out",
                "x.parquet",
                "--json",
                "--engine",
                NO_ENGINE,
            ],
            "export (engine): --json",
        ),
        pin(
            &["data", "hist", "export", "--out", "x.parquet", "--engine", NO_ENGINE, SPEC],
            "export (engine): flags before the positional",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out=x.parquet",
                "--engine=no-such-engine",
                "--from=2024-01-01T00",
            ],
            "export (engine): inline spellings",
        ),
        // The remote route.
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out",
                "out.jsonl",
                "--addr",
                DEAD,
                "--format",
                "jsonl",
                "--from",
                "2024-01-01",
                "--to",
                "2024-01-31",
            ],
            "usage.rs: export --addr --format jsonl",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out",
                "out.csv",
                "--addr",
                DEAD,
                "--format",
                "csv",
                "--from",
                "2024-01-01",
                "--to",
                "2024-01-31",
            ],
            "usage.rs: export --addr --format csv",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                "binance:BTCUSDT",
                "--kind",
                "quote",
                "--out",
                "out.jsonl",
                "--addr",
                DEAD,
                "--format",
                "jsonl",
                "--from",
                "2024-01-01",
                "--to",
                "2024-01-02",
            ],
            "export (remote): --kind quote takes VENUE:SYMBOL",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                "binance:BTCUSDT",
                "--kind",
                "trade",
                "--out",
                "out.jsonl",
                "--addr",
                DEAD,
                "--format",
                "jsonl",
                "--from",
                "2024-01-01",
                "--to",
                "2024-01-02",
            ],
            "export (remote): --kind trade",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--kind",
                "bar",
                "--out",
                "out.jsonl",
                "--addr",
                DEAD,
                "--format",
                "jsonl",
                "--from",
                "2024-01-01",
                "--to",
                "2024-01-31",
                "--window",
                "7d",
            ],
            "export (remote): --kind bar and --window 7d",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out",
                "out.jsonl",
                "--addr",
                DEAD,
                "--format",
                "jsonl",
                "--from",
                "1704067200000",
                "--to",
                "1706745600000",
                "--window",
                "4h",
                "--json",
            ],
            "export (remote): epoch-ms bounds, --window 4h, --json",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out=out.jsonl",
                "--addr=127.0.0.1:1",
                "--format=jsonl",
                "--from=2024-01-01",
                "--to=2024-01-31",
                "--window=1d",
            ],
            "export (remote): inline spellings of every flag",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                "--addr",
                DEAD,
                "--out",
                "out.jsonl",
                "--format",
                "jsonl",
                "--to",
                "2024-01-31",
                "--from",
                "2024-01-01",
                SPEC,
            ],
            "export (remote): flags before the positional, --to before --from",
        ),
    ]);
}

/// `rm`: both routes, the selector flags, the confirmation pair and the provenance assertion.
#[test]
fn data_hist_rm() {
    assert_accepted(&[
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1h",
                "--addr",
                DEAD,
                "--dry-run",
            ],
            "rm (remote): symbol + interval, a rehearsal",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "book",
                "--venue",
                "polymarket",
                "--group",
                "btc-5m",
                "--addr",
                DEAD,
                "--dry-run",
            ],
            "rm (remote): --group",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--produced-by",
                "panel_bars:",
                "--addr",
                DEAD,
                "--dry-run",
            ],
            "rm (remote): a sweep (wildcard symbol) with --produced-by",
        ),
        pin(
            &[
                "data", "hist", "rm", "--kind", "bar", "--venue", "binance", "--symbol", "BTCUSDT",
                "--addr", DEAD, "--yes",
            ],
            "rm (remote): --yes (the dial is refused, so nothing is deleted)",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--addr",
                DEAD,
                "--dry-run",
                "--json",
            ],
            "rm (remote): --json",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--addr",
                DEAD,
                "--dry-run",
                "--format",
                "json",
            ],
            "rm (remote): --format json",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind=bar",
                "--venue=binance",
                "--symbol=BTCUSDT",
                "--interval=1h",
                "--addr=127.0.0.1:1",
                "--dry-run",
            ],
            "rm (remote): inline spellings",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1h",
                "--store",
                "store",
                "--engine",
                NO_ENGINE,
                "--dry-run",
            ],
            "rm (local): --store and --engine",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--engine",
                NO_ENGINE,
                "--dry-run",
            ],
            "rm (local): no --store",
        ),
        pin(
            &[
                "data", "hist", "rm", "--kind", "bar", "--venue", "binance", "--symbol", "BTCUSDT",
                "--store", "store", "--engine", NO_ENGINE, "--yes",
            ],
            "rm (local): --yes (the engine is absent, so nothing is deleted)",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "demo",
                "--produced-by",
                "crates/vike-data/src/demo.rs",
                "--store",
                "store",
                "--engine",
                NO_ENGINE,
                "--dry-run",
            ],
            "rm (local): --produced-by may be a producer PATH on the local route",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "book",
                "--venue",
                "polymarket",
                "--group",
                "btc-5m",
                "--store",
                "store",
                "--engine",
                NO_ENGINE,
                "--dry-run",
                "--json",
            ],
            "rm (local): --group, --json",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind=bar",
                "--venue=binance",
                "--symbol=BTCUSDT",
                "--store=store",
                "--engine=no-such-engine",
                "--dry-run",
            ],
            "rm (local): inline spellings",
        ),
    ]);
}

/// `repair`: engine-only, rehearses by default.
#[test]
fn data_hist_repair() {
    assert_accepted(&[
        pin(
            &[
                "data",
                "hist",
                "repair",
                "--kind",
                "book",
                "--venue",
                "polymarket",
                "--group",
                "btc-5m",
                "--store",
                "store",
                "--engine",
                NO_ENGINE,
            ],
            "docs/ops/datahub-the CI box.md: repair, the default rehearsal",
        ),
        pin(
            &[
                "data",
                "hist",
                "repair",
                "--kind",
                "book",
                "--venue",
                "polymarket",
                "--group",
                "btc-5m",
                "--store",
                "store",
                "--engine",
                NO_ENGINE,
                "--yes",
            ],
            "docs/ops/datahub-the CI box.md: repair --yes",
        ),
        pin(
            &[
                "data",
                "hist",
                "repair",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1h",
                "--store",
                "store",
                "--engine",
                NO_ENGINE,
                "--dry-run",
            ],
            "repair: a bar series needs symbol + interval",
        ),
        pin(
            &[
                "data",
                "hist",
                "repair",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1h",
                "--engine",
                NO_ENGINE,
                "--dry-run",
                "--yes",
            ],
            "repair: --dry-run beside --yes (dry-run wins) and no --store",
        ),
        pin(
            &[
                "data",
                "hist",
                "repair",
                "--kind",
                "book",
                "--venue",
                "polymarket",
                "--group",
                "btc-5m",
                "--store",
                "store",
                "--engine",
                NO_ENGINE,
                "--json",
            ],
            "repair: --json",
        ),
        pin(
            &[
                "data",
                "hist",
                "repair",
                "--kind=book",
                "--venue=polymarket",
                "--group=btc-5m",
                "--store=store",
                "--engine=no-such-engine",
            ],
            "repair: inline spellings",
        ),
    ]);
}

/// The few read lines that name NO `--addr`, so they dial the default datahub address.
///
/// Kept apart on purpose: on a dev box with the prod tunnel up, `127.0.0.1:7878` can be a live
/// datahub. Every line here is READ-ONLY (a listing), and without node keys a keyed datahub refuses
/// it at the handshake; drop this test, not the others, if that is not acceptable.
#[test]
fn data_hist_reads_on_the_default_address() {
    assert_accepted(&[
        pin(
            &["data", "hist", "ls", "--venue", "oanda"],
            "docs/ops/oanda-history.md: ls --venue oanda",
        ),
        pin(
            &["data", "hist", "ls", "--venue", "dukascopy"],
            "docs/ops/dukascopy-archive-import.md",
        ),
        pin(
            &["data", "hist", "ls", "--addr", "127.0.0.1:7878", "--json"],
            "docs/ops/recorder-deploy.md: the row-count check",
        ),
        pin(&["data", "hist", "running"], "get-market-data skill: running"),
    ]);
}

// ─── data catalog ────────────────────────────────────────────────────────────────────────────────

#[test]
fn data_catalog() {
    assert_accepted(&[
        pin(
            &["data", "catalog", "ls", "--venue", "binance", "--addr", DEAD],
            "catalog ls: --venue",
        ),
        pin(
            &[
                "data",
                "catalog",
                "ls",
                "--venue",
                "binance",
                "--class",
                "CryptoPerp",
                "--addr",
                DEAD,
            ],
            "catalog ls: --class takes a VALUE here (a bare switch on `data hist ls`)",
        ),
        pin(
            &[
                "data",
                "catalog",
                "ls",
                "--venue",
                "binance",
                "--class",
                "cryptoperp",
                "--addr",
                DEAD,
            ],
            "catalog ls: --class is case-insensitive",
        ),
        pin(
            &["data", "catalog", "ls", "--venue", "oanda", "--class", "fx", "--addr", DEAD],
            "catalog ls: --class fx (any case of a stored word)",
        ),
        pin(
            &[
                "data",
                "catalog",
                "ls",
                "--venue",
                "polymarket",
                "--class",
                "PredictionMarket",
                "--addr",
                DEAD,
            ],
            "catalog ls: a multi-word class",
        ),
        pin(
            &["data", "catalog", "ls", "--venue", "binance", "--search", "btc", "--addr", DEAD],
            "catalog ls: --search",
        ),
        pin(
            &[
                "data",
                "catalog",
                "ls",
                "--venue",
                "binance",
                "--search",
                "perp usdt",
                "--addr",
                DEAD,
            ],
            "catalog ls: --search with a space (one argv element)",
        ),
        pin(
            &[
                "data", "catalog", "ls", "--venue", "binance", "--class", "Fx", "--search", "eur",
                "--addr", DEAD, "--json",
            ],
            "catalog ls: every flag",
        ),
        pin(
            &["data", "catalog", "ls", "--venue", "binance", "--addr", DEAD, "--format", "json"],
            "catalog ls: --format json",
        ),
        pin(
            &["data", "catalog", "ls", "--venue", "binance", "--addr", DEAD, "--format", "table"],
            "catalog ls: --format table",
        ),
        pin(
            &[
                "data",
                "catalog",
                "ls",
                "--venue=binance",
                "--class=Fx",
                "--search=eur",
                "--addr=127.0.0.1:1",
                "--format=json",
            ],
            "catalog ls: inline spellings",
        ),
        pin(
            &["data", "catalog", "ls", "--addr", DEAD, "--json", "--venue", "binance"],
            "catalog ls: flag order is free",
        ),
        pin(
            &["data", "catalog", "show", "binance:BTCUSDT", "--addr", DEAD],
            "catalog show: VENUE:SYMBOL",
        ),
        pin(
            &["data", "catalog", "show", "binance:BTCUSDT", "--addr", DEAD, "--json"],
            "catalog show: --json",
        ),
        pin(
            &["data", "catalog", "show", "binance:BTCUSDT", "--addr", DEAD, "--format", "json"],
            "catalog show: --format json",
        ),
        pin(
            &["data", "catalog", "show", "--addr", DEAD, "binance:BTCUSDT"],
            "catalog show: flags before the positional",
        ),
        pin(
            &["data", "catalog", "show", "binance:BTCUSDT", "--addr=127.0.0.1:1", "--format=json"],
            "catalog show: inline spellings",
        ),
        pin(
            &["data", "catalog", "refresh", "--venue", "binance", "--addr", DEAD],
            "catalog refresh: --venue",
        ),
        pin(
            &["data", "catalog", "refresh", "--venue", "binance", "--addr", DEAD, "--json"],
            "catalog refresh: --json",
        ),
        pin(
            &["data", "catalog", "refresh", "--venue=binance", "--addr=127.0.0.1:1"],
            "catalog refresh: inline spellings",
        ),
        pin(
            &["data", "catalog", "venues", "--addr", DEAD],
            "catalog venues: an unreachable datahub is REPORTED, not fatal",
        ),
        pin(&["data", "catalog", "venues", "--addr", DEAD, "--json"], "catalog venues: --json"),
        pin(
            &["data", "catalog", "venues", "--addr", DEAD, "--format", "json"],
            "catalog venues: --format json",
        ),
        pin(
            &["data", "catalog", "venues", "--addr", DEAD, "--format", "table"],
            "catalog venues: --format table",
        ),
        pin(&["data", "catalog", "venues", "--addr=127.0.0.1:1"], "catalog venues: inline --addr"),
    ]);
}

// ─── data realtime ───────────────────────────────────────────────────────────────────────────────

#[test]
fn data_realtime() {
    assert_accepted(&[
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "depth",
                "--for",
                "30s",
                "--addr",
                DEAD,
            ],
            "watch: --lane depth --for 30s",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "book",
                "--events",
                "10",
                "--addr",
                DEAD,
            ],
            "watch: --lane book --events N",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "trades",
                "--unbounded",
                "--addr",
                DEAD,
            ],
            "watch: --lane trades --unbounded",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "depth",
                "--depth",
                "20",
                "--for",
                "5m",
                "--events",
                "100",
                "--addr",
                DEAD,
            ],
            "watch: --depth with both bounds",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "depth",
                "--depth",
                "500",
                "--for",
                "2h",
                "--addr",
                DEAD,
            ],
            "watch: a --depth above the wire ceiling is clamped by the server, not refused",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "book",
                "--for",
                "2H",
                "--addr",
                DEAD,
            ],
            "watch: --for units are case-insensitive (except M)",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "book",
                "--for",
                "30S",
                "--addr",
                DEAD,
            ],
            "watch: --for 30S",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "trades",
                "--events",
                "5",
                "--format",
                "jsonl",
                "--addr",
                DEAD,
            ],
            "watch: --format jsonl",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "trades",
                "--events",
                "5",
                "--format",
                "table",
                "--addr",
                DEAD,
            ],
            "watch: --format table",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "trades",
                "--events",
                "5",
                "--out",
                "out.jsonl",
                "--addr",
                DEAD,
            ],
            "watch: --out FILE",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "hyperliquid:@107",
                "--lane",
                "trades",
                "--events",
                "5",
                "--addr",
                DEAD,
            ],
            "watch: a symbol beginning with @ is a symbol, not a family marker",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "--lane",
                "depth",
                "--for",
                "30s",
                "--addr",
                DEAD,
                "binance:BTCUSDT",
            ],
            "watch: flags before the positional key",
        ),
        pin(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane=depth",
                "--events=5",
                "--for=1m",
                "--depth=10",
                "--addr=127.0.0.1:1",
            ],
            "watch: inline spellings of every valued flag",
        ),
        pin(&["data", "realtime", "status", "--addr", DEAD], "status: --addr"),
        pin(&["data", "realtime", "status", "--addr", DEAD, "--json"], "status: --json"),
        pin(
            &["data", "realtime", "status", "--addr", DEAD, "--format", "json"],
            "status: --format json",
        ),
        pin(
            &["data", "realtime", "status", "--addr", DEAD, "--format", "table"],
            "status: --format table",
        ),
        pin(
            &["data", "realtime", "status", "--addr=127.0.0.1:1", "--format=json"],
            "status: inline spellings",
        ),
    ]);
}

// ─── data realtime record ────────────────────────────────────────────────────────────────────────

/// `record` edits the sandbox's own settings database; with no recorder profile stored every line
/// ends in a refused precondition (exit 1), never in the usage rung.
#[test]
fn data_realtime_record() {
    assert_accepted(&[
        pin(&["data", "realtime", "record", "ls"], "record ls"),
        pin(
            &["data", "realtime", "record", "ls", "--profile", "default"],
            "record ls: --profile NAME",
        ),
        pin(&["data", "realtime", "record", "ls", "--profiles"], "record ls: --profiles"),
        pin(&["data", "realtime", "record", "ls", "--json"], "record ls: --json"),
        pin(&["data", "realtime", "record", "ls", "--format", "json"], "record ls: --format json"),
        pin(
            &["data", "realtime", "record", "ls", "--format", "table"],
            "record ls: --format table",
        ),
        pin(
            &["data", "realtime", "record", "ls", "--profile=default", "--format=json"],
            "record ls: inline spellings",
        ),
        pin(&["data", "realtime", "record", "add", "binance:BTCUSDT"], "record add: VENUE:SYMBOL"),
        pin(
            &["data", "realtime", "record", "add", "binance:BTCUSDT.P", "--dry-run"],
            "record add: a dotted symbol, --dry-run",
        ),
        pin(
            &["data", "realtime", "record", "add", "polymarket:@btc-updown-5m", "--dry-run"],
            "record add: VENUE:@FAMILY (@ marks a whole family HERE)",
        ),
        pin(
            &[
                "data",
                "realtime",
                "record",
                "add",
                "binance:BTCUSDT",
                "--backfill",
                "venue",
                "--note",
                "x y",
                "--dry-run",
            ],
            "record add: --backfill venue and --note with a space",
        ),
        pin(
            &["data", "realtime", "record", "add", "binance:BTCUSDT", "--backfill", "archive"],
            "record add: --backfill archive",
        ),
        pin(
            &["data", "realtime", "record", "add", "binance:BTCUSDT", "--backfill", "off"],
            "record add: --backfill off",
        ),
        pin(
            &[
                "data",
                "realtime",
                "record",
                "add",
                "binance:BTCUSDT",
                "--profile",
                "default",
                "--dry-run",
            ],
            "record add: --profile",
        ),
        pin(
            &[
                "data",
                "realtime",
                "record",
                "add",
                "--backfill=off",
                "--note=-leading-dash",
                "--profile=default",
                "--dry-run",
                "binance:BTCUSDT",
            ],
            "record add: inline spellings, a --note beginning with a single dash, flags before the spec",
        ),
        pin(&["data", "realtime", "record", "rm", "binance:BTCUSDT"], "record rm: VENUE:SYMBOL"),
        pin(
            &["data", "realtime", "record", "rm", "binance:BTCUSDT", "--ord", "0", "--dry-run"],
            "record rm: --ord N",
        ),
        pin(
            &[
                "data",
                "realtime",
                "record",
                "rm",
                "polymarket:@btc-updown-5m",
                "--profile",
                "default",
                "--dry-run",
            ],
            "record rm: a family, --profile",
        ),
        pin(
            &[
                "data",
                "realtime",
                "record",
                "rm",
                "--ord=1",
                "--profile=default",
                "binance:BTCUSDT",
            ],
            "record rm: inline spellings, flags before the spec",
        ),
    ]);
}

// ─── data source ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn data_source() {
    assert_accepted(&[
        pin_code(&["data", "source", "ls"], 0, "source ls"),
        pin_code(&["data", "source", "ls", "--json"], 0, "source ls: --json"),
        pin_code(&["data", "source", "ls", "--format", "json"], 0, "source ls: --format json"),
        pin_code(
            &["data", "source", "ls", "--format=table"],
            0,
            "source ls: --format table, inline",
        ),
        pin_code(
            &["data", "source", "show", "binance"],
            0,
            "get-market-data skill: source show VENUE",
        ),
        pin_code(
            &["data", "source", "show", "oanda"],
            0,
            "get-market-data skill: a credentialed venue",
        ),
        pin_code(&["data", "source", "show", "starter"], 0, "source show: a built source"),
        pin_code(&["data", "source", "show", "demo"], 0, "source show: the demo source"),
        pin_code(
            &["data", "source", "show", "recorder"],
            0,
            "source show: a designed (unbuilt) source",
        ),
        pin_code(&["data", "source", "show", "tardis"], 0, "source show: a designed vendor source"),
        pin_code(&["data", "source", "show", "vike"], 0, "source show: the vike source"),
        pin_code(
            &["data", "source", "show", "some-venue-nobody-knows"],
            0,
            "source show: any other word is a venue class",
        ),
        pin_code(
            &["data", "source", "show", "a=b"],
            0,
            "source show: a positional containing = is kept whole",
        ),
        pin_code(
            &["data", "source", "show", "binance", "--json"],
            0,
            "source show: --json after the name",
        ),
        pin_code(
            &["data", "source", "show", "--json", "binance"],
            0,
            "source show: --json before the name",
        ),
        pin_code(
            &["data", "source", "show", "binance", "--format", "json"],
            0,
            "source show: --format json",
        ),
        pin(
            &["data", "source", "show", "binance", "--addr", DEAD],
            "source show: --addr asks one datahub (a roster venue)",
        ),
        pin(
            &["data", "source", "show", "binance", "--addr=127.0.0.1:1", "--json"],
            "source show: inline --addr, --json",
        ),
        pin(
            &["data", "source", "show", "--addr", DEAD, "oanda"],
            "source show: --addr before the name",
        ),
    ]);
}

// ─── trade: status, halt, resume, and the REPL entry ─────────────────────────────────────────────

/// `status` / `halt` / `resume`: three node-wide words with no group. With no node key in the
/// sandbox each ends at exit 1 before a socket opens.
#[test]
fn trade_status_halt_resume() {
    assert_accepted(&[
        pin(&["trade", "status", "--node", DEAD], "docs/ops: trade status --node"),
        pin(&["trade", "status", "--node", DEAD, "--json"], "trade status: --json"),
        pin(&["trade", "status", "--json", "--node", DEAD], "trade status: --json first"),
        pin(&["trade", "status", "--node=127.0.0.1:1", "--json"], "trade status: inline --node"),
        pin(
            &["trade", "status", "--node", "127.0.0.1:2", "--node", DEAD],
            "trade status: a repeated --node, last wins",
        ),
        pin(
            &[
                "trade",
                "halt",
                "--node",
                DEAD,
                "--yes",
                "--reason",
                "manual stop, incident 2026-09-10",
            ],
            "docs/ops/tradehub-container.md: halt with a one-argument reason",
        ),
        pin(&["trade", "halt", "--node", DEAD, "--yes"], "trade halt: --yes"),
        pin(&["trade", "halt", "--node", DEAD, "-y"], "trade halt: -y"),
        pin(&["trade", "halt", "--node", DEAD], "trade halt: no --yes"),
        pin(
            &["trade", "halt", "--yes", "--reason", "x", "--node", DEAD],
            "trade halt: --reason before --node",
        ),
        pin(
            &["trade", "halt", "--node=127.0.0.1:1", "--yes", "--reason=manual stop"],
            "trade halt: inline --node and --reason",
        ),
        pin(
            &["trade", "halt", "--node", DEAD, "--yes", "--reason", "-urgent"],
            "trade halt: a --reason beginning with a single dash is a value",
        ),
        pin(
            &["trade", "halt", "--node", DEAD, "--yes", "-y", "--yes"],
            "trade halt: --yes and -y repeated",
        ),
        pin(&["trade", "resume", "--node", DEAD], "docs/ops: trade resume --node"),
        pin(&["trade", "resume", "--node", DEAD, "--yes"], "trade resume: --yes"),
        pin(
            &["trade", "resume", "--node", DEAD, "-y", "--reason", "back to active"],
            "trade resume: -y and --reason",
        ),
        pin(
            &["trade", "resume", "--node=127.0.0.1:1", "--reason=back"],
            "trade resume: inline spellings",
        ),
    ]);
}

/// The bare `trade` entry opens the REPL (stdin is closed, and no key resolves, so it ends at exit 1).
#[test]
fn trade_repl_flags() {
    assert_accepted(&[
        pin(&["trade", "--node", DEAD], "README / docs/ops: vike-cli trade --node ADDR"),
        pin(&["trade", "--node=127.0.0.1:1"], "trade: inline --node"),
        pin(&["trade", "--node", DEAD, "--yes"], "trade: --yes skips the per-write confirm"),
        pin(&["trade", "--node", DEAD, "-y"], "trade: -y"),
        pin(&["trade", "-y", "--node", DEAD], "trade: -y first (a leading flag is not a group)"),
        pin(&["trade", "--yes", "--node=127.0.0.1:1"], "trade: --yes first, inline --node"),
        pin(
            &["trade", "--node", "127.0.0.1:2", "--node", DEAD],
            "trade: a repeated --node, last wins",
        ),
        pin(&["trade", "--node", DEAD, "--yes", "--yes", "-y"], "trade: repeated switches"),
    ]);
}

// ─── trade order ─────────────────────────────────────────────────────────────────────────────────

/// `trade order ls` (a read) and the four writes. The writes end at exit 1 (no control key); the
/// wrapper flags `--node/--yes/-y/--json/--reason/-h` are recognised ANYWHERE among the arguments,
/// but only in the `--flag value` spelling (`--node=ADDR` is not a wrapper flag there).
#[test]
fn trade_order() {
    assert_accepted(&[
        pin(&["trade", "order", "ls", "--node", DEAD], "order ls: --node"),
        pin(
            &["trade", "order", "ls", "binance", "--node", DEAD],
            "order ls: a bare-venue book FIRST",
        ),
        pin(
            &["trade", "order", "ls", "binance/DEFAULT", "--node", DEAD],
            "order ls: the DEFAULT account spelled out",
        ),
        pin(
            &["trade", "order", "ls", "binance/ALT", "--node", DEAD],
            "order ls: a labelled book is refused later (exit 4), not by the reader",
        ),
        pin(
            &["trade", "order", "ls", "binance", "--node", DEAD, "--symbol", "BTCUSDT", "--json"],
            "order ls: --symbol --json",
        ),
        pin(&["trade", "order", "ls", "--node", DEAD, "--symbol", "BTCUSDT"], "order ls: --symbol"),
        pin(
            &[
                "trade",
                "order",
                "ls",
                "binance",
                "--node=127.0.0.1:1",
                "--symbol=BTCUSDT",
                "--json",
            ],
            "order ls: inline spellings",
        ),
        pin(&["trade", "order", "ls", "--json", "--node", DEAD], "order ls: --json first"),
        // submit
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "buy", "0.01", "@50000",
                "--node", DEAD, "--yes",
            ],
            "docs/ops: order submit ... --node ... --yes (a limit order)",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "buy", "0.01", "--node", DEAD,
                "--yes",
            ],
            "submit: no @price is a market order",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "sell", "2", "@market", "--node",
                DEAD, "--yes",
            ],
            "submit: @market",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "BUY", "0.01", "--node", DEAD,
                "-y",
            ],
            "submit: the side is case-insensitive; -y",
        ),
        pin(
            &["trade", "order", "submit", "binance", "BTCUSDT", "b", "1", "--node", DEAD, "--yes"],
            "submit: side word b",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "long", "1", "--node", DEAD,
                "--yes",
            ],
            "submit: side word long",
        ),
        pin(
            &["trade", "order", "submit", "binance", "BTCUSDT", "s", "1", "--node", DEAD, "--yes"],
            "submit: side word s",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "short", "1", "--node", DEAD,
                "--yes",
            ],
            "submit: side word short",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "buy", "1e-3", "@1.5e4",
                "--node", DEAD, "--yes",
            ],
            "submit: float syntax in qty and price",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "@100", "buy", "1", "--node",
                DEAD, "--yes",
            ],
            "submit: the @price may sit among the positionals",
        ),
        pin(
            &[
                "trade",
                "order",
                "submit",
                "binance/ALT",
                "BTCUSDT",
                "sell",
                "2",
                "@market",
                "--reduce-only",
                "--node",
                DEAD,
                "--yes",
            ],
            "submit: a labelled book and --reduce-only",
        ),
        pin(
            &[
                "trade",
                "order",
                "submit",
                "binance/DEFAULT",
                "BTCUSDT",
                "buy",
                "1",
                "--node",
                DEAD,
                "--yes",
            ],
            "submit: binance/DEFAULT",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "buy", "1", "--coid", "abc123",
                "--node", DEAD, "--yes",
            ],
            "submit: --coid ID",
        ),
        pin(
            &[
                "trade",
                "order",
                "submit",
                "binance",
                "BTCUSDT",
                "buy",
                "1",
                "--coid=abc123",
                "--node",
                DEAD,
                "--yes",
            ],
            "submit: --coid=ID is the one inline wrapper-side spelling",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "buy", "1", "--node", DEAD,
                "--yes", "--json",
            ],
            "submit: --json",
        ),
        pin(
            &[
                "trade", "order", "submit", "--node", DEAD, "--yes", "binance", "BTCUSDT", "buy",
                "1", "@50000",
            ],
            "submit: wrapper flags before the positionals",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "--node", DEAD, "BTCUSDT", "buy", "--yes",
                "1",
            ],
            "submit: wrapper flags between the positionals",
        ),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "buy", "1", "--node", DEAD,
                "--yes", "--reason", "scale in",
            ],
            "submit: --reason LAST, unquoted words",
        ),
        pin(
            &[
                "trade",
                "order",
                "submit",
                "binance",
                "BTCUSDT",
                "buy",
                "1",
                "--node",
                DEAD,
                "--yes",
                "--reason",
                "scale in, tranche 2",
            ],
            "submit: --reason LAST, one quoted argument",
        ),
        pin(
            &[
                "trade",
                "order",
                "submit",
                "binance",
                "BTCUSDT",
                "buy",
                "1",
                "--node",
                "127.0.0.1:2",
                "--node",
                DEAD,
                "--yes",
            ],
            "submit: a repeated --node, last wins",
        ),
        // cancel
        pin(&["trade", "order", "cancel", "abc123", "--node", DEAD, "--yes"], "cancel: COID"),
        pin(
            &["trade", "order", "cancel", "--node", DEAD, "--yes", "abc123"],
            "cancel: flags before the coid",
        ),
        pin(
            &[
                "trade",
                "order",
                "cancel",
                "abc123",
                "--node",
                DEAD,
                "-y",
                "--json",
                "--reason",
                "fat finger",
            ],
            "cancel: -y --json --reason",
        ),
        // modify
        pin(
            &["trade", "order", "modify", "abc123", "--qty", "2", "--node", DEAD, "--yes"],
            "modify: --qty",
        ),
        pin(
            &["trade", "order", "modify", "abc123", "--price", "100.5", "--node", DEAD, "--yes"],
            "modify: --price",
        ),
        pin(
            &[
                "trade", "order", "modify", "abc123", "--qty", "2", "--price", "100.5", "--node",
                DEAD, "--yes", "--json",
            ],
            "modify: --qty and --price",
        ),
        pin(
            &["trade", "order", "modify", "--node", DEAD, "--yes", "abc123", "--price", "1"],
            "modify: wrapper flags before the coid",
        ),
        pin(
            &["trade", "order", "modify", "abc123", "--price", "-5", "--node", DEAD, "--yes"],
            "modify: a negative price is a value (a single-dash token)",
        ),
        // mass-cancel
        pin(
            &["trade", "order", "mass-cancel", "--node", DEAD, "--yes"],
            "mass-cancel: no book is the widest, risk-reducing form",
        ),
        pin(
            &["trade", "order", "mass-cancel", "binance", "--node", DEAD, "--yes"],
            "mass-cancel: a bare venue",
        ),
        pin(
            &["trade", "order", "mass-cancel", "binance", "BTCUSDT", "--node", DEAD, "--yes"],
            "mass-cancel: venue and symbol",
        ),
        pin(
            &[
                "trade",
                "order",
                "mass-cancel",
                "binance/ALT",
                "BTCUSDT",
                "--node",
                DEAD,
                "-y",
                "--json",
                "--reason",
                "flat",
            ],
            "mass-cancel: a labelled book",
        ),
        pin(
            &["trade", "order", "mass-cancel", "--node", DEAD, "--yes", "binance"],
            "mass-cancel: flags before the book",
        ),
    ]);
}

// ─── trade position ──────────────────────────────────────────────────────────────────────────────

#[test]
fn trade_position() {
    assert_accepted(&[
        pin(&["trade", "position", "ls", "--node", DEAD], "position ls: --node"),
        pin(
            &["trade", "position", "ls", "binance", "--node", DEAD],
            "position ls: a bare-venue book first",
        ),
        pin(
            &["trade", "position", "ls", "binance/ALT", "--node", DEAD],
            "position ls: a labelled book",
        ),
        pin(
            &[
                "trade", "position", "ls", "binance", "--node", DEAD, "--symbol", "BTCUSDT",
                "--json",
            ],
            "position ls: --symbol --json",
        ),
        pin(
            &["trade", "position", "ls", "--node=127.0.0.1:1", "--symbol=BTCUSDT", "--json"],
            "position ls: inline spellings",
        ),
        pin(
            &["trade", "position", "flatten", "binance", "BTCUSDT", "--node", DEAD, "--yes"],
            "flatten: BOOK SYMBOL",
        ),
        pin(
            &[
                "trade",
                "position",
                "flatten",
                "binance/ALT",
                "BTCUSDT",
                "--node",
                DEAD,
                "-y",
                "--json",
            ],
            "flatten: a labelled book",
        ),
        pin(
            &["trade", "position", "flatten", "--node", DEAD, "--yes", "binance", "BTCUSDT"],
            "flatten: wrapper flags before the positionals",
        ),
        pin(
            &[
                "trade", "position", "flatten", "binance", "BTCUSDT", "--node", DEAD, "--yes",
                "--reason", "risk off",
            ],
            "flatten: --reason LAST",
        ),
        pin(
            &["trade", "position", "close-all", "--node", DEAD, "--yes"],
            "close-all: no book is the panic button",
        ),
        pin(
            &["trade", "position", "close-all", "binance", "--node", DEAD, "--yes"],
            "close-all: one book",
        ),
        pin(
            &["trade", "position", "close-all", "binance/DEFAULT", "--node", DEAD, "-y", "--json"],
            "close-all: binance/DEFAULT",
        ),
        pin(
            &[
                "trade",
                "position",
                "close-all",
                "--node",
                DEAD,
                "--yes",
                "--reason",
                "incident 2026-09-10",
            ],
            "close-all: --reason LAST",
        ),
    ]);
}

// ─── trade strategy ──────────────────────────────────────────────────────────────────────────────

#[test]
fn trade_strategy() {
    assert_accepted(&[
        pin(&["trade", "strategy", "ls", "--node", DEAD], "strategy ls: --node"),
        pin(&["trade", "strategy", "ls", "--node", DEAD, "--json"], "strategy ls: --json"),
        pin(
            &["trade", "strategy", "ls", "--node=127.0.0.1:1", "--json"],
            "strategy ls: inline --node",
        ),
        pin(
            &[
                "trade", "strategy", "mount", "binance", "BTCUSDT", "1m", "--name", "grid",
                "--node", DEAD, "--yes",
            ],
            "mount: a registry --name",
        ),
        pin(
            &[
                "trade",
                "strategy",
                "mount",
                "binance",
                "BTCUSDT",
                "1m",
                "--rhai",
                "/srv/vike/strat.rhai",
                "--node",
                DEAD,
                "--yes",
            ],
            "mount: a --rhai path on the node",
        ),
        pin(
            &[
                "trade", "strategy", "mount", "binance", "BTCUSDT", "1m", "--name", "grid", "--id",
                "grid1", "--node", DEAD, "--yes",
            ],
            "mount: --id",
        ),
        pin(
            &[
                "trade",
                "strategy",
                "mount",
                "binance",
                "BTCUSDT",
                "1m",
                "--name",
                "grid",
                "--account",
                "ALT",
                "--node",
                DEAD,
                "--yes",
            ],
            "mount: --account LABEL",
        ),
        pin(
            &[
                "trade",
                "strategy",
                "mount",
                "binance",
                "BTCUSDT",
                "1m",
                "--name",
                "grid",
                "--account",
                "DEFAULT",
                "--node",
                DEAD,
                "--yes",
            ],
            "mount: --account DEFAULT",
        ),
        pin(
            &[
                "trade",
                "strategy",
                "mount",
                "binance",
                "BTCUSDT",
                "1m",
                "--name",
                "grid",
                "--params",
                r#"{"gamma": 0.0008}"#,
                "--node",
                DEAD,
                "--yes",
            ],
            "mount: --params is ONE quoted JSON argument",
        ),
        pin(
            &[
                "trade",
                "strategy",
                "mount",
                "binance",
                "BTCUSDT",
                "1m",
                "--name=grid",
                "--id=grid1",
                "--account=ALT",
                "--params={\"gamma\":0.0008}",
                "--node",
                DEAD,
                "--yes",
            ],
            "mount: the inline spellings of every mount flag",
        ),
        pin(
            &[
                "trade", "strategy", "mount", "--name", "grid", "--node", DEAD, "--yes", "binance",
                "BTCUSDT", "1m",
            ],
            "mount: every flag before the positionals",
        ),
        pin(
            &[
                "trade", "strategy", "mount", "binance", "BTCUSDT", "1m", "--name", "grid",
                "--node", DEAD, "-y", "--json", "--reason", "go live",
            ],
            "mount: -y --json --reason LAST",
        ),
        pin(&["trade", "strategy", "unmount", "grid1", "--node", DEAD, "--yes"], "unmount: ID"),
        pin(
            &[
                "trade",
                "strategy",
                "unmount",
                "binance__BTCUSDT__1m",
                "--node",
                DEAD,
                "-y",
                "--json",
                "--reason",
                "retire",
            ],
            "unmount: the node-derived id, -y --json --reason",
        ),
        pin(
            &["trade", "strategy", "unmount", "--node", DEAD, "--yes", "grid1"],
            "unmount: flags before the id",
        ),
    ]);
}

// ─── -h / --help on every verb path ──────────────────────────────────────────────────────────────

/// Every verb path the two planes route, each a prefix of a real command line.
const HELP_PATHS: &[&[&str]] = &[
    &["data"],
    &["data", "hist"],
    &["data", "hist", "fetch"],
    &["data", "hist", "running"],
    &["data", "hist", "cancel"],
    &["data", "hist", "import"],
    &["data", "hist", "export"],
    &["data", "hist", "get"],
    &["data", "hist", "ls"],
    &["data", "hist", "gaps"],
    &["data", "hist", "coverage"],
    &["data", "hist", "health"],
    &["data", "hist", "universe"],
    &["data", "hist", "gate"],
    &["data", "hist", "rm"],
    &["data", "hist", "repair"],
    &["data", "catalog"],
    &["data", "catalog", "ls"],
    &["data", "catalog", "show"],
    &["data", "catalog", "refresh"],
    &["data", "catalog", "venues"],
    &["data", "realtime"],
    &["data", "realtime", "watch"],
    &["data", "realtime", "status"],
    &["data", "realtime", "record"],
    &["data", "realtime", "record", "ls"],
    &["data", "realtime", "record", "add"],
    &["data", "realtime", "record", "rm"],
    &["data", "source"],
    &["data", "source", "ls"],
    &["data", "source", "show"],
    &["trade"],
    &["trade", "status"],
    &["trade", "halt"],
    &["trade", "resume"],
    &["trade", "order"],
    &["trade", "order", "ls"],
    &["trade", "order", "submit"],
    &["trade", "order", "cancel"],
    &["trade", "order", "modify"],
    &["trade", "order", "mass-cancel"],
    &["trade", "position"],
    &["trade", "position", "ls"],
    &["trade", "position", "flatten"],
    &["trade", "position", "close-all"],
    &["trade", "strategy"],
    &["trade", "strategy", "ls"],
    &["trade", "strategy", "mount"],
    &["trade", "strategy", "unmount"],
];

/// `path` followed by one more argument, leaked so it can live in a `Pin` (a test process, so the
/// leak is a few dozen small slices).
fn with_flag(path: &[&'static str], flag: &'static str) -> &'static [&'static str] {
    let mut argv = path.to_vec();
    argv.push(flag);
    Box::leak(argv.into_boxed_slice())
}

#[test]
fn help_on_every_verb_path() {
    let mut pins: Vec<Pin> = Vec::new();
    for path in HELP_PATHS {
        for flag in ["-h", "--help"] {
            pins.push(pin_code(
                with_flag(path, flag),
                0,
                "-h / --help prints the usage and succeeds",
            ));
        }
    }
    assert_accepted(&pins);
}

/// The word `help` is a group's help spelling, and `-h` / `--help` also work mid-line.
#[test]
fn the_help_word_and_help_amid_other_flags() {
    assert_accepted(&[
        pin_code(&["data", "help"], 0, "data help"),
        pin_code(&["data", "hist", "help"], 0, "data hist help"),
        pin_code(&["data", "catalog", "help"], 0, "data catalog help"),
        pin_code(&["data", "realtime", "help"], 0, "data realtime help"),
        pin_code(&["data", "realtime", "record", "help"], 0, "data realtime record help"),
        pin_code(&["data", "source", "help"], 0, "data source help"),
        pin_code(&["trade", "order", "help"], 0, "trade order help"),
        pin_code(&["trade", "position", "help"], 0, "trade position help"),
        pin_code(&["trade", "strategy", "help"], 0, "trade strategy help"),
        pin_code(&["data", "hist", "get", SPEC, "--help"], 0, "--help after a positional"),
        pin_code(&["data", "hist", "ls", "--help", "--addr", DEAD], 0, "--help before other flags"),
        pin_code(
            &["data", "hist", "fetch", SPEC, "--days", "30", "-h"],
            0,
            "-h after a complete fetch line",
        ),
        pin_code(
            &["data", "catalog", "ls", "--venue", "binance", "--help"],
            0,
            "catalog ls: --help after a flag",
        ),
        pin_code(
            &["data", "realtime", "watch", "binance:BTCUSDT", "--lane", "depth", "-h"],
            0,
            "watch: -h after the spec and lane",
        ),
        pin_code(&["trade", "--node", DEAD, "--help"], 0, "the REPL: --help after --node"),
        pin_code(&["trade", "-h", "--node", DEAD], 0, "the REPL: -h before --node"),
        pin_code(&["trade", "status", "--node", DEAD, "-h"], 0, "trade status: -h after --node"),
        pin_code(
            &["trade", "halt", "--node", DEAD, "--yes", "--help"],
            0,
            "trade halt: --help after --yes",
        ),
        pin_code(
            &["trade", "order", "submit", "binance", "BTCUSDT", "buy", "1", "--help"],
            0,
            "submit: --help amid a full line",
        ),
        pin_code(&["trade", "order", "ls", "binance", "-h"], 0, "order ls: -h after the book"),
        pin_code(
            &["trade", "strategy", "mount", "--help", "binance"],
            0,
            "mount: --help among the positionals",
        ),
    ]);
}

// ─── refusals that must STAY refused ─────────────────────────────────────────────────────────────

/// Retired verbs and spellings the parsers answer BY NAME (exit 2 with the replacement), and the
/// flags a verb refuses on purpose. A rewrite that starts accepting one of these is a regression too.
#[test]
fn retired_and_named_refusals_stay_refused() {
    assert_accepted(&[
        // pre-group `data` verbs
        pin_code(&["data"], 2, "no bare `data`: a group is required"),
        pin_code(&["data", "ls"], 2, "retired: data ls is data hist ls"),
        pin_code(&["data", "list"], 2, "retired: data list"),
        pin_code(
            &["data", "fetch", SPEC, "--days", "30"],
            2,
            "retired: data fetch is data hist fetch",
        ),
        pin_code(&["data", "fetch-starter"], 2, "retired: data fetch-starter"),
        pin_code(&["data", "seed-demo"], 2, "retired: data seed-demo"),
        pin_code(&["data", "export", SPEC, "--out", "x.parquet"], 2, "retired: data export"),
        pin_code(&["data", "coverage"], 2, "retired: data coverage"),
        pin_code(&["data", "tape-health"], 2, "retired: data tape-health"),
        pin_code(&["data", "universe"], 2, "retired: data universe"),
        pin_code(&["data", "rm", "--kind", "bar", "--venue", "binance"], 2, "retired: data rm"),
        pin_code(
            &["data", "repair", "--kind", "bar", "--venue", "binance"],
            2,
            "retired: data repair",
        ),
        pin_code(&["data", "nonsense"], 2, "an unknown group"),
        // renamed or absorbed `data hist` verbs
        pin_code(&["data", "hist"], 2, "data hist needs a verb"),
        pin_code(&["data", "hist", "list"], 2, "retired: data hist list is data hist ls"),
        pin_code(
            &["data", "hist", "tape-health"],
            2,
            "retired: data hist tape-health is data hist health",
        ),
        pin_code(&["data", "hist", "fetch-starter"], 2, "retired: data hist fetch-starter"),
        pin_code(&["data", "hist", "seed-demo"], 2, "retired: data hist seed-demo"),
        pin_code(&["data", "hist", "nonsense"], 2, "an unknown data hist verb"),
        // flags answered by name
        pin_code(
            &["data", "hist", "ls", "--gaps", "--addr", DEAD],
            2,
            "retired: ls --gaps is the gaps verb",
        ),
        pin_code(
            &["data", "hist", "ls", "--store", "store"],
            2,
            "a read verb refuses --store (decision 0084)",
        ),
        pin_code(
            &["data", "hist", "export", SPEC, "--out", "x.parquet", "--store", "store"],
            2,
            "export refuses --store",
        ),
        pin_code(
            &["data", "hist", "ls", "--format", "jsonl", "--addr", DEAD],
            2,
            "jsonl is get's, not a catalog verb's",
        ),
        pin_code(
            &["data", "hist", "ls", "--limit", "5", "--addr", DEAD],
            2,
            "--limit belongs to get alone",
        ),
        pin_code(&["data", "hist", "ls", "--bogus"], 2, "an unknown option"),
        pin_code(
            &["data", "hist", "fetch", SPEC, "--days", "30", "--store", "store"],
            2,
            "a venue fetch refuses --store and --engine",
        ),
        pin_code(
            &["data", "hist", "fetch", "--source", "demo", "--addr", DEAD, "--engine", NO_ENGINE],
            2,
            "starter/demo refuse --addr",
        ),
        pin_code(
            &["data", "hist", "fetch", "--source", "tardis"],
            2,
            "an unbuilt source is refused by name",
        ),
        pin_code(
            &["data", "hist", "fetch", "--source", "recorder"],
            2,
            "recorder is a verb group, not a source",
        ),
        pin_code(&["data", "hist", "fetch", SPEC], 2, "a venue fetch needs a window"),
        pin_code(
            &[
                "data",
                "hist",
                "fetch",
                SPEC,
                "--days",
                "30",
                "--from",
                "2024-01-01",
                "--addr",
                DEAD,
            ],
            2,
            "--days and --from are two ways to say one thing",
        ),
        pin_code(
            &[
                "data",
                "hist",
                "repair",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1h",
                "--addr",
                DEAD,
            ],
            2,
            "repair has no --addr",
        ),
        pin_code(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--addr",
                DEAD,
                "--store",
                "store",
                "--dry-run",
            ],
            2,
            "rm: --addr and --store are two stores",
        ),
        // the sub-groups
        pin_code(&["data", "catalog", "list"], 2, "retired: data catalog list is data catalog ls"),
        pin_code(&["data", "catalog"], 2, "data catalog needs a verb"),
        pin_code(&["data", "catalog", "ls", "--addr", DEAD], 2, "catalog ls needs --venue"),
        pin_code(&["data", "realtime"], 2, "data realtime needs a verb"),
        pin_code(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "quotes",
                "--for",
                "5s",
                "--addr",
                DEAD,
            ],
            2,
            "a lane the wire does not serve is refused by name",
        ),
        pin_code(
            &["data", "realtime", "watch", "binance:BTCUSDT", "--lane", "depth", "--addr", DEAD],
            2,
            "watch is bounded by default: it needs --for/--events/--unbounded",
        ),
        pin_code(&["data", "realtime", "record", "status"], 2, "record status does not exist"),
        pin_code(
            &["data", "realtime", "record", "add", "binance:BTCUSDT", "--addr", DEAD],
            2,
            "record --addr is designed and refused by name",
        ),
        pin_code(
            &["data", "realtime", "record", "add", "binance:BTCUSDT", "--lane", "depth"],
            2,
            "record has no --lane grain",
        ),
        pin_code(&["data", "source", "ls", "--addr", DEAD], 2, "source ls reaches no server"),
        pin_code(
            &["data", "source", "ls", "--store", "store"],
            2,
            "a data hist flag on data source is refused by name",
        ),
        pin_code(&["data", "source", "show"], 2, "source show needs a name"),
        // trade
        pin_code(
            &["trade", "state", "halted", "--node", DEAD],
            2,
            "retired: trade state halted is trade halt",
        ),
        pin_code(&["trade", "state"], 2, "retired: trade state"),
        pin_code(
            &["trade", "submit", "binance", "BTCUSDT", "buy", "1", "--node", DEAD],
            2,
            "retired REPL word: submit is trade order submit",
        ),
        pin_code(
            &["trade", "buy", "binance", "BTCUSDT", "1", "--node", DEAD],
            2,
            "retired REPL word: buy",
        ),
        pin_code(
            &["trade", "orders", "--node", DEAD],
            2,
            "retired REPL word: orders is trade order ls",
        ),
        pin_code(
            &["trade", "positions", "--node", DEAD],
            2,
            "retired REPL word: positions is trade position ls",
        ),
        pin_code(
            &["trade", "flatten", "binance", "BTCUSDT", "--node", DEAD],
            2,
            "retired REPL word: flatten is trade position flatten",
        ),
        pin_code(
            &["trade", "panic", "--node", DEAD],
            2,
            "retired REPL word: panic is trade position close-all",
        ),
        pin_code(
            &["trade", "mount", "--node", DEAD],
            2,
            "retired REPL word: mount is trade strategy mount",
        ),
        pin_code(&["trade", "account"], 2, "an unbuilt group is refused by name"),
        pin_code(&["trade", "watch"], 2, "an unbuilt group is refused by name"),
        pin_code(&["trade", "nonsense"], 2, "an unknown group"),
        pin_code(&["trade"], 2, "the REPL needs --node"),
        pin_code(&["trade", "status"], 2, "trade status needs --node"),
        pin_code(&["trade", "halt"], 2, "trade halt needs --node"),
        pin_code(&["trade", "order"], 2, "trade order needs a verb"),
        pin_code(&["trade", "order", "ls"], 2, "trade order ls needs --node"),
        pin_code(
            &["trade", "order", "submit", "binance", "BTCUSDT", "buy", "0.01", "--yes"],
            2,
            "a one-shot write needs --node",
        ),
        pin_code(
            &["trade", "order", "submit", "BTCUSDT", "buy", "0.01", "--node", DEAD, "--yes"],
            2,
            "submit needs a book first (three positionals is the old grammar)",
        ),
        pin_code(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "buy", "0", "--node", DEAD,
                "--yes",
            ],
            2,
            "an order that cannot be sized is a usage error",
        ),
        pin_code(
            &["trade", "order", "ls", "binance#ALT", "--node", DEAD],
            2,
            "a route key is not a book selector",
        ),
        pin_code(
            &["trade", "order", "ls", "@mark", "--node", DEAD],
            2,
            "a mark is a designed-not-built form",
        ),
        pin_code(
            &["trade", "strategy", "mount", "binance", "BTCUSDT", "1m", "--node", DEAD, "--yes"],
            2,
            "mount needs --name XOR --rhai",
        ),
        pin_code(
            &[
                "trade", "strategy", "mount", "binance", "BTCUSDT", "1m", "--name", "a", "--rhai",
                "b", "--node", DEAD, "--yes",
            ],
            2,
            "--name and --rhai are exclusive",
        ),
    ]);
}

// ─── the lines the docs, skills and scripts print ────────────────────────────────────────────────

/// Command lines copied from `README.md`, `docs/ops/`, the shipped skills and `scripts/`, with
/// their placeholders filled and every dial/engine pointed at the dead sandbox targets.
#[test]
fn documented_command_lines() {
    assert_accepted(&[
        pin(
            &["data", "hist", "fetch", SPEC, "--days", "180", "--addr", DEAD],
            "scripts/mirror_readme.md: fetch --days 180",
        ),
        pin(
            &[
                "data",
                "hist",
                "fetch",
                "oanda:EUR_USD:5s",
                "--from",
                "2005-01-03",
                "--to",
                "2005-12-31",
                "--addr",
                DEAD,
            ],
            "docs/ops/oanda-history.md: a year window",
        ),
        pin(
            &[
                "data",
                "hist",
                "fetch",
                "oanda:EUR_USD:5s",
                "--from",
                "2006-01-01",
                "--to",
                "2006-12-31",
                "--addr",
                DEAD,
            ],
            "docs/ops/oanda-history.md: the next year",
        ),
        pin(
            &[
                "data",
                "hist",
                "fetch",
                SPEC,
                "--from",
                "2026-01-01",
                "--to",
                "2026-02-01",
                "--addr",
                DEAD,
            ],
            "skills/run-a-backtest-locally.md: fetch --from --to",
        ),
        pin(
            &[
                "data",
                "hist",
                "fetch",
                "okx:BTC-USDT:1h",
                "--from",
                "2024-01-01",
                "--to",
                "2024-06-01",
                "--addr",
                DEAD,
            ],
            "skills/get-market-data.md (its --addr <host>:7878 swapped for the dead port)",
        ),
        pin(
            &[
                "data",
                "hist",
                "fetch",
                "dukascopy:EURUSD:7m",
                "--from",
                "2024-01-01",
                "--to",
                "2024-01-31",
                "--addr",
                DEAD,
            ],
            "docs/ops/dukascopy-archive-import.md: a derived bar step",
        ),
        pin(
            &["data", "hist", "fetch", "--source", "demo", "--engine", NO_ENGINE],
            "README: fetch --source demo (an engine named so none is located)",
        ),
        pin(
            &["data", "hist", "fetch", "--source", "starter", "--engine", NO_ENGINE],
            "README: fetch --source starter (an engine named so none is located)",
        ),
        pin(
            &[
                "data", "hist", "fetch", "--source", "demo", "--engine", NO_ENGINE, "--store",
                "store",
            ],
            "scripts/cli_mcp_smoke.sh",
        ),
        pin(&["data", "hist", "running", "--addr", DEAD], "skills/get-market-data.md: running"),
        pin(
            &["data", "hist", "cancel", "oanda:EUR_USD:5s", "--addr", DEAD],
            "docs/ops/oanda-history.md: cancel",
        ),
        pin(
            &["data", "hist", "import", "dukascopy-bi5", "EURUSD", "--dry-run", "--addr", DEAD],
            "docs/ops/dukascopy-archive-import.md: the plan",
        ),
        pin(
            &[
                "data",
                "hist",
                "import",
                "dukascopy-bi5",
                "EURUSD",
                "--dry-run",
                "--verify",
                "--addr",
                DEAD,
            ],
            "docs/ops/dukascopy-archive-import.md: the verify pass",
        ),
        pin(
            &[
                "data",
                "hist",
                "import",
                "dukascopy-bi5",
                "EURUSD",
                "--from",
                "2024-01-01",
                "--to",
                "2024-01-31",
                "--yes",
                "--addr",
                DEAD,
            ],
            "skills/get-market-data.md: one month, confirmed",
        ),
        pin(
            &["data", "hist", "export", SPEC, "--out", "btc-1h.parquet", "--engine", NO_ENGINE],
            "skills/get-market-data.md",
        ),
        pin(
            &[
                "data",
                "hist",
                "export",
                SPEC,
                "--out",
                "btc-1h.parquet",
                "--from",
                "2024-01-01T00",
                "--engine",
                NO_ENGINE,
            ],
            "skills/get-market-data.md: an hour label",
        ),
        pin(
            &["data", "hist", "ls", "--venue", "oanda", "--addr", DEAD],
            "docs/ops/oanda-history.md: ls --venue",
        ),
        pin(
            &["data", "hist", "ls", "--venue", "dukascopy", "--addr", DEAD],
            "docs/ops/dukascopy-archive-import.md: ls --venue",
        ),
        pin(
            &["data", "hist", "ls", "--addr", DEAD, "--json"],
            "docs/ops/recorder-deploy.md: the row-count check (its 127.0.0.1:7878 swapped for the dead port)",
        ),
        pin(
            &[
                "data",
                "hist",
                "gate",
                SPEC,
                "--require-days",
                "365",
                "--max-gap",
                "1d",
                "--addr",
                DEAD,
            ],
            "data hist gate's own usage line",
        ),
        pin(
            &[
                "data",
                "hist",
                "rm",
                "--kind",
                "bar",
                "--venue",
                "demo",
                "--symbol",
                "DEMOUSDT",
                "--interval",
                "1h",
                "--store",
                "store",
                "--engine",
                NO_ENGINE,
                "--dry-run",
            ],
            "docs/ops/datahub-the CI box.md: the local delete route",
        ),
        pin(
            &[
                "data",
                "hist",
                "repair",
                "--kind",
                "book",
                "--venue",
                "polymarket",
                "--group",
                "btc-5m",
                "--store",
                "store",
                "--engine",
                NO_ENGINE,
            ],
            "docs/ops/datahub-the CI box.md: repair, rehearsal",
        ),
        pin(
            &[
                "data",
                "hist",
                "repair",
                "--kind",
                "book",
                "--venue",
                "polymarket",
                "--group",
                "btc-5m",
                "--store",
                "store",
                "--engine",
                NO_ENGINE,
                "--yes",
            ],
            "docs/ops/datahub-the CI box.md: repair, performed",
        ),
        pin_code(
            &["data", "source", "show", "oanda"],
            0,
            "docs/ops/oanda-history.md: source show VENUE",
        ),
        pin(
            &["data", "realtime", "record", "add", "binance:BTCUSDT.P", "--dry-run"],
            "deploy/vike-datahub.service: record add",
        ),
        pin(
            &["trade", "--node", DEAD],
            "README / docs/ops: the REPL (its port 7879 swapped for the dead port)",
        ),
        pin(
            &["trade", "status", "--node", DEAD],
            "scripts/cli_mcp_smoke.sh / docs/ops: trade status",
        ),
        pin(
            &[
                "trade",
                "halt",
                "--node",
                DEAD,
                "--yes",
                "--reason",
                "manual stop, incident 2026-09-10",
            ],
            "docs/ops/tradehub-container.md: halt",
        ),
        pin(&["trade", "resume", "--node", DEAD], "docs/ops/tradehub-container.md: resume"),
        pin(
            &[
                "trade", "order", "submit", "binance", "BTCUSDT", "buy", "0.01", "@50000",
                "--node", DEAD, "--yes",
            ],
            "docs/ops/tradehub-container.md: order submit ... --node ... --yes",
        ),
    ]);
}

/// Command lines that WERE accepted by the hand-rolled readers and are REFUSED now, each by a row of
/// decision 0155's rule table (verdict 3): a flag given twice that is not repeatable used to
/// overwrite silently (last wins) and is refused with `clap`'s wording; a switch given a value, a
/// single-dash token that is not a number as a value, and every usage error's exit code (2, where
/// `config` used to exit 1) are the other rows. They are pinned here as REFUSED so a rewrite cannot
/// make them silently accepted again. Moved out of the accepted tests above, unchanged otherwise.
#[test]
fn what_decision_0155_refuses_now() {
    assert_accepted(&[
        pin_code(
            &[
                "data", "hist", "ls", "--addr", DEAD, "--kind", "bar", "--kind", "trade", "--json",
                "--json",
            ],
            2,
            "ls: a repeated flag is tolerated, the last value wins",
        ),
        pin_code(
            &[
                "data",
                "hist",
                "universe",
                "--addr",
                DEAD,
                "--from",
                "2024-01-01",
                "--from",
                "2024-02-01",
            ],
            2,
            "universe: a repeated --from, last wins",
        ),
        pin_code(
            &["data", "hist", "get", SPEC, "--days", "7", "--days", "9", "--addr", DEAD],
            2,
            "get: a repeated --days, last wins",
        ),
        pin_code(
            &["data", "hist", "fetch", SPEC, "--days", "30", "--days", "60", "--addr", DEAD],
            2,
            "fetch: a repeated --days, last wins",
        ),
        pin_code(
            &[
                "data",
                "hist",
                "rm",
                "--dry-run",
                "--dry-run",
                "--yes",
                "--kind",
                "bar",
                "--venue",
                "binance",
                "--engine",
                NO_ENGINE,
            ],
            2,
            "rm: repeated bare switches are tolerated",
        ),
        pin_code(
            &["data", "catalog", "ls", "--venue", "binance", "--search", "-perp", "--addr", DEAD],
            2,
            "catalog ls: a --search needle that starts with a single dash is a value",
        ),
        pin_code(
            &["data", "catalog", "ls", "--venue", "binance", "--venue", "okx", "--addr", DEAD],
            2,
            "catalog ls: a repeated --venue, last wins",
        ),
        pin_code(
            &[
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT",
                "--lane",
                "depth",
                "--lane",
                "book",
                "--events",
                "5",
                "--addr",
                DEAD,
            ],
            2,
            "watch: a repeated --lane, last wins",
        ),
    ]);
}
