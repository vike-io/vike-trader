//! `backtest`'s operator-facing TEXT: every usage page, and the spellings they advertise.

use super::{DATA_SUBCOMMAND, TRIALS_SUBCOMMAND};

/// What `--help` prints. A const rather than the module doc above, because only a const can reach a
/// user: that doc is for a reader of this file, `backtest --help` is for everyone else.
pub(super) const USAGE: &str = "\
usage: backtest PROFILE.toml [--json]
       backtest PARAMSCAN.toml [--rank-by sharpe|return|max_dd|equity|multi]
                [--optimizer grid|euler|tpe|genetic]
                [--euler-depth N] [--trials N] [--seed S]
                [--keep-trials none|scalars|returns] [--resume ID]
                [--min-trades N] [--progress auto|none|json]
       backtest --addr [HOST:PORT]
       backtest --list
       backtest --list-optimizers [--json]
       backtest data <subcommand> [options]     (see `backtest data --help`)
       backtest trials <search-run-id> [options] (see `backtest trials --help`)

  --addr [ADDR]    STAY ALIVE and serve the seven COMPUTE verbs over the node protocol —
                   RunBacktest, RunSlice, RunParamscan, RunWalkforward, RunParamscanProfile,
                   RunWalkforwardProfile, ListStrategies. The VALUE IS OPTIONAL: a bare --addr
                   serves on the configured address (VIKE_BACKTEST_ADDR, then
                   config.backtest_addr, then 127.0.0.1:7880); --addr HOST:PORT overrides it.
                   The FLAG is what says 'become a daemon' — no profile is ever implied.
                   A non-loopback address is refused unless VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 is
                   set AND node keys are configured: this server compiles Rhai the client sends.
                   The DATA verbs (LoadBars, ListSeries, Backfill, ...) live on the other daemon,
                   `vike-backend datahub`, and are refused here by name
  PROFILE.toml     the harness profile TOML (data slice + engine costs + strategy), as the first
                   POSITIONAL argument. REQUIRED unless --list. A profile with a [paramscan]
                   table — or its permanent [sweep] alias — runs a parameter search instead of
                   one backtest.
  --profile PATH   the same thing, the older spelling. KEPT: `vike-cli backtest run --local` SPAWNS
                   this binary with it. Giving BOTH is an error — they may name two different
                   files, so it is refused rather than resolved.
  --store DIR      REFUSED on the run path since 2026-09-25: history is read from a datahub.
                   For local files, start a key-less one on them first:
                   VIKE_DATAHUB_STORE=DIR vike-backend datahub
  --archive PATH   read DOWNLOADED `data.vike.io` archive Parquet IN PLACE instead of an imported
                   store — a directory of day files, or one file. No import step: the import is
                   measured at ~16 s per 999k-row row group on a flat-layout day carrying 300 of
                   them, because each group splits across as many destination partitions as it
                   holds distinct token_ids. Refused together with --store: two different stores,
                   and a silent precedence rule is how somebody backtests the wrong data. A run
                   this way records `fingerprint: null` — the part-file witness needs a
                   DataFusionHist store, and the reason is printed
  --json           print the report as pretty JSON instead of the human table
  --list           print every registered strategy name and exit — no store, no profile needed
  --list-optimizers
                   print every parameter-search METHOD, one per line, and exit — no store, no
                   profile, no network. With --json, one object carrying the count and which
                   method runs when --optimizer is absent. The `--optimizer` twin of --list
  data <sub>       get data INTO the store, out of it, and OUT of existence. The operator-facing
                   spelling is `vike-cli data hist <verb>`, which spawns this; on a box with only the
                   engine, `backtest data <sub>` is the same thing typed directly. It replaced
                   the five flags --seed-demo/--fetch/--fetch-starter/--export/--rm-series, each
                   of which is now refused by name
  trials <id>      list the trials of a finished (or interrupted) parameter SEARCH. Reads the run
                   ARTIFACT only — no store, no profile, no network
  --rank-by M      paramscan ranking metric (default sharpe); `multi` is the composite objective.
                   Ignored — not an error — on a profile with no [paramscan] table
  --optimizer M    the parameter-search METHOD: grid (default, the exhaustive cartesian product),
                   euler (bounded successive-halving refinement), tpe (Bayesian ask/tell) or
                   genetic (a population search over the grid's own points, which needs --seed).
                   Replaces the retired --search, which is now an error naming this flag
  --euler-depth N  euler halving depth (default 3). EULER ONLY — refused, not discarded, under
                   another method; past the cap it is refused rather than silently clamped
  --trials N       tpe trial budget (default 64). TPE ONLY — refused under another method
  --seed S         the reproducibility seed. TPE AND GENETIC — refused under grid and euler. On
                   tpe it defaults to 0; on genetic it is REQUIRED, because a genetic run reports
                   one sample of a distribution and a seed nobody typed is a constant the answer
                   silently depends on
  --keep-trials K  what a SEARCH leaves behind: `scalars` (default) writes one ledger line per
                   trial under <project>/user_data/runs/<id>/trials.jsonl; `none` keeps the parent
                   run and no ledger; `returns` is `scalars` PLUS a bucketed return vector kept
                   per trial in memory, which adds the anti-overfitting statistics (PBO via CSCV,
                   effective trial count, deflated Sharpe) to that run's report.json as
                   `overfit.*` — the keys `backtest gate --fail-if` can name. `series` (a whole
                   equity curve per trial) is still refused by name, on retention grounds; use
                   `returns` for the statistics, or re-run the winning point for a curve
  --resume ID      continue an interrupted SEARCH: re-open that parent run, reuse every trial it
                   already holds, and evaluate only what is missing. Refused if the profile, the
                   store, the optimizer, the seed or the budget differ from the run being resumed
  --min-trades N   the statistical-significance FLOOR: a trial that closed fewer than N trades is
                   UNRANKABLE rather than merely penalised, under every method and every
                   --rank-by. 0 (the default, and also what an explicit `--min-trades 0` means)
                   disarms it, so a sweep matrix can pass the flag unconditionally
  --progress M     which progress stream a SEARCH emits, on stderr and never stdout: auto (the
                   default — the throttled human line, and ONLY when stderr is a terminal), none
                   (silent, nothing counted), json (one newline-delimited event per point,
                   terminal or not, unthrottled)
  -h, --help       print this and exit 0
  -V, --version    print the version and exit 0";

/// What `backtest data --help` prints — the five operations ruling 12 moved, as one subcommand.
///
/// A const of its own rather than five more rows in [`USAGE`], because that is the shape of the
/// move: `backtest --help` now names ONE line where it named five, and the detail lives with the
/// verb that carries it. The wording mirrors `crates/vike-cli/src/cmd/data/usage.rs`'s `USAGE` on
/// purpose — the same five operations, spelled the same way, reached by whichever of the two
/// binaries the operator has.
pub(super) const DATA_USAGE: &str = "\
usage: backtest data fetch-starter [--store DIR]
       backtest data seed-demo [--store DIR]
       backtest data export VENUE:SYMBOL:INTERVAL --out FILE [--from LABEL] [--to LABEL]
       backtest data rm --kind K --venue V (--symbol S [--interval I] | --group G)
                [--produced-by PREFIX] [--dry-run] [--yes] [--store DIR] [--json]
       backtest data repair --kind K --venue V (--symbol S [--interval I] | --group G)
                [--yes] [--dry-run] [--store DIR] [--json]

⚠ `vike-cli data hist <verb>` is the operator-facing spelling and takes the same words; it SPAWNS
  this binary, because writing a hist store and encoding Parquet need DataFusion and that binary
  links none. Use this one directly on a box that has an engine and no vike-cli.

  fetch SPEC       RETIRED — this binary reaches no venue. It refuses and names the replacement,
                   `vike-cli data hist fetch VENUE:SYMBOL:INTERVAL --days N [--addr HOST:PORT]`,
                   which asks a DATAHUB to fetch the window into the store. ⚠ Not a drop-in:
                   that needs a reachable datahub, and this command needed no server at all
  fetch-starter    download the PUBLISHED starter dataset (real bars, plain HTTPS, no venue and
                   no credentials) and load it. For a box a venue cannot be reached from —
                   a geoblock, a locked-down network. Verified against its published SHA256SUMS;
                   safe to re-run. Behind the `venue-fetch` feature, whose name now outlives the
                   venue call it was created for — see `crate::starter`
  seed-demo        write the SYNTHETIC demo tape into the store and exit. Venue `demo`, a
                   closed-form curve, NOT market data; it is the slice the shipped
                   `user_data/profiles/backtest.toml` names, so a fresh install can run that
                   profile immediately. Safe to re-run: a second seed writes nothing
  export SPEC      write one series to a standalone Parquet file (--out FILE), optionally
                   bounded by --from/--to — INDEPENDENTLY, unlike fetch's window: an export
                   slices what the store already holds, so one bound alone is meaningful and
                   neither is required. Any venue the store holds, `demo` included.
                   ⚠ It READS through a datahub like every history reader (decision 0084): the
                   bars come from the datahub the settings name — config.datahub_addr, or
                   $VIKE_DATAHUB_ADDR above it, loopback by default — and only the Parquet
                   encoding happens here. `--store` is
                   REFUSED on it; for files on this machine start a key-less datahub on them
                   first: VIKE_DATAHUB_STORE=DIR vike-backend datahub
  rm               DELETE stored series, IRREVERSIBLY, and exit. Selects on the four series
                   dimensions: --kind and --venue are REQUIRED, and an omitted
                   --symbol/--group/--interval is a wildcard over that dimension. Prints the
                   PLAN first — the resolved store root and the rung that chose it, then every
                   matched series with its rows/bytes/days and the commit keys that wrote it.
                   --produced-by asserts that EVERY key of EVERY matched series carries that
                   prefix (a producer path from STORE_KINDS resolves to its prefix); one foreign
                   key refuses the whole run and deletes nothing. It is REQUIRED for a sweep and
                   optional for a fully-named series. --dry-run stops after the plan; otherwise
                   --yes, or type `delete N series` at a terminal. There is no --force
  repair           REBUILD one series' manifest from its parts — the repair `read_manifest`
                   names when a series' index is missing or unreadable. Selects ONE series
                   exactly (never a wildcard: --symbol or --group is REQUIRED, and --interval
                   too on `bar`), because the failure it fixes is invisible to every
                   enumeration this store has. REHEARSES BY DEFAULT: it prints what the rebuild
                   would recover and what it would LOSE, writes nothing, takes no lock, and
                   exits 0. --yes performs it; --dry-run wins over --yes. A rebuild that
                   recovers the index but not the idempotency log exits NON-ZERO and says so.
                   It REFUSES while another writer holds the series lock, and never waits

  --store DIR      the WRITERS' hist-store root (fetch-starter, seed-demo, rm, repair); falls
                   back to $VIKE_HIST_STORE, then <repo>/market_data/hist. Refused on `export`
  --json           on `rm` and `repair`, print the plan/outcome document instead of the human
                   rendering
  -h, --help       print this and exit 0";

/// The optimizer-listing spelling, typed ONCE: [`run`]'s arm reads it, [`USAGE`] advertises it, and
/// `the_usage_spells_every_progress_mode_the_parser_accepts` holds the two together while
/// `the_optimizer_listing_flag_is_the_spelling_the_vocabulary_declares` holds it against
/// `vike_datahub_client::flag_vocab`'s row. Three surfaces name this flag and none of them may
/// spell it differently — an arm reading `--list-optimizer` would be a flag nothing can reach while
/// every other check stayed green.
pub(super) const LIST_OPTIMIZERS_FLAG: &str = "--list-optimizers";

pub(super) const TRIALS_USAGE: &str = "\
usage: backtest trials <search-run-id> [--sort FIELD] [--top N] [--json]
                [--export-params FILE]

  <search-run-id>  a FULL run id, as `backtest: search saved to …` printed it and as the run
                   directory under <project>/user_data/runs/ is named. Prefixes, `@last` and
                   marks are the design's shared SELECTOR GRAMMAR and are not implemented here
  --sort FIELD     score (default) | n | return | sharpe | max_dd | trades | equity.
                   `score` and `n` come from the ledger; the rest are read out of each trial's
                   recorded metrics. A trial with no metric for the field sorts LAST
  --top N          print only the first N rows after sorting. The document's counts still
                   describe the WHOLE search
  --json           print the run's own TrialsDocument instead of the table — the same schema
                   report.json holds
  --export-params FILE
                   write the top-sorted trial's overrides as a [strategy.params] TOML fragment
  -h, --help       print this and exit 0";

/// The words that are SUBCOMMANDS here and therefore can never be read as the positional profile:
/// `(word, what follows it)`. Ruling 12's `data` and stage 5's `trials` collide with ruling 14's
/// positional profile on exactly one token each, and [`run`] resolves that by routing only when the
/// word comes FIRST. This table is the OTHER half — the refusal for the case that reaches
/// [`profile_from_args`].
pub(super) const SUBCOMMANDS: &[(&str, &str)] = &[
    (DATA_SUBCOMMAND, "<fetch|fetch-starter|seed-demo|export|rm|repair>"),
    (TRIALS_SUBCOMMAND, "<id>"),
];
