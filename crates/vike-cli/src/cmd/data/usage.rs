//! `vike-cli data`'s long help: the prose of every verb, which `Cli`'s `long_about` prints under
//! `data --help`. The option rows are NOT here any more — each flag's description is the `help` of
//! its `HistFlags` field (`hist.rs`), so `--help`, the parser and the published reference are one
//! description.

// History, kept so a citation by symbol still lands: the hand-kept `USAGE` text this file held is
// `long_about` (the prose) plus each flag's own `help` now (decision 0155).

/// What `vike-cli data --help` says about the verbs: groups, then each verb's essay. The options
/// table is clap's, generated from `HistFlags`. The datahub's default address is vike-config's,
/// filled in where the page says `{datahub}`.
pub(super) fn long_about() -> String {
    LONG_ABOUT_PAGE.replace("{datahub}", vike_config::DEFAULT_DATAHUB_ADDRESS)
}

/// [`long_about`]'s page: a `const` that opens `"\`, which is how this crate marks a help page whose
/// newlines are its layout (`tests/cli_verbs/message_literals.rs`).
const LONG_ABOUT_PAGE: &str = "\
Get market data into the hist store the backtest engine reads, and ask a running
vike-datahub what is already in one. The two halves reach DIFFERENT stores and their
flags do not mix — each half's flags are refused on the other, by name.

GROUPS — every verb below lives in one, and the group is REQUIRED: there is no bare
`vike-cli data <verb>`, and each pre-group spelling is refused by name with its
replacement rather than silently accepted:
  history      WHEN = a past window. The verbs on THIS page
  realtime     WHEN = now (watch | status | live-tokens | record) — `data realtime --help`. `record`
               is a SUB-GROUP of its own (ls | add | rm): what the BOX persists, as
               rows in this project's settings database. Its --addr half — editing
               ANOTHER box's rows — is designed and refused by name
  catalog      what is addressable (ls | show | refresh | venues) — `data catalog --help`
  source       where rows may come from (ls | show) — `data source --help`, and it
               reaches no server: every answer says what it did NOT check
  instruments  the OPERATOR's own instruments, added by hand (add | rm | ls) — `data
               instruments --help`: a venue's list does not carry them, the datahub
               keeps them in its settings database and folds them into the kept lists;
               `refresh` has the datahub fetch and keep the venues' own lists now
  splits       the OPERATOR's share splits (add | rm | rename | ls | warnings) — `data splits --help`: one
               row per split of one stored series, in this box's settings database; a
               backtest divides the series' prices before each split by its ratio. `warnings`
               asks the datahub which share series jumped overnight with no split row

FETCH — asks a DATAHUB, which is the only thing that reaches a venue:
  fetch SPEC   ask the datahub to pull REAL bars into its store. SPEC is
               VENUE:SYMBOL:INTERVAL (e.g. binance:BTCUSDT:1h). Needs a window:
               --days, or --from/--to. Most venues need no credentials — it is public
               market data — and a venue that needs one is marked: `vike-cli data source
               show VENUE` says what a venue needs and how far back each of its doors goes.
               VENUE:SYMBOL:funding fetches a perpetual's funding-rate series (a binance
               perpetual is SYMBOL.P, e.g. binance:BTCUSDT.P:funding). SYMBOL is the
               instrument's name as this workspace spells it on every venue (oanda:EURUSD,
               ig:EURUSD, dukascopy:AAPL.CFD), never the venue's own (EUR_USD, an epic):
               the datahub finds the venue's spelling on its kept instrument list.
               ⚠ Needs a reachable datahub (--addr, default {datahub}): history
               is fetched by the backend, once, into the store. It takes no --store
               and no --engine, and refuses them by name rather than ignoring them.
               A window LONGER than a year, at a venue whose datahub lane stores whole
               UTC days (the lane `data source show VENUE` calls CredentialedKlines),
               is sent as one request per calendar year, cut at 1 January 00:00 UTC,
               with a line on stderr as each finishes; a failed year stops the run,
               the years before it stay stored, and re-running resumes. Every other
               window is ONE request: those lanes key what they store by the request's
               own bounds, so cutting it would store the same rows twice

A RUNNING FETCH — asks the datahub about the fetches it is serving right now. A fetch
whose client goes away (Ctrl-C, a closed tunnel) stops on its own at its next chunk
boundary; these are for one you cannot reach — left running in another session or on
another machine, or behind a half-open connection:
  running      every fetch the datahub is running: its SERIES (the spec, ready to
               paste into `cancel`), its window, how long it has run, the chunk
               BOUNDARIES it has reached (chunks begun — not a percentage), the peer
               that sent it, and whether a cancel can stop it
  cancel SPEC  stop every running fetch on SPEC (VENUE:SYMBOL:INTERVAL, exactly as it
               was fetched) at its next chunk boundary. Nothing is removed: the chunk
               in flight finishes, every chunk before the boundary stays stored, and
               repeating that fetch resumes there — so there is no confirmation. It
               does not wait: the stopped fetch's own client is answered with an
               error naming the cancel, and `running` lists it until it has stopped.
               A ONE-BATCH lane (a keyless kline venue, a funding series) fetches its
               window in one request and cannot be stopped: the cancel names it and it
               runs to its end. `cancel` needs this box's device key on that
               datahub's access list with the control role or higher, `running` the
               observe role or higher. Against a datahub older than these verbs both
               are refused by name with nothing sent — restarting it is then the only
               stop

IMPORT — asks a DATAHUB to read a vendor ARCHIVE from a folder on the DATAHUB's OWN box into
its store; for Dukascopy's, --fetch first has that datahub DOWNLOAD the window into the
folder with YOUR AWS key stored on its box, billed to your AWS account:
  import FORMAT DATASET
               read ONE dataset of an archive FORMAT the datahub advertises — today
               dukascopy-bi5, Dukascopy's daily tick files (--fetch downloads them;
               the plan and its estimated AWS cost come first), and binance-aggtrades,
               Binance's public daily aggTrades zips — from
               <project>/market_data/imports/FORMAT/DATASET/ ON THE DATAHUB'S BOX.
               It stores the ticks and derives bars per imported day (--bars, default
               1m; for binance-aggtrades default none, and any it derives go to the
               source=aggtrades lane, never the candles). DATASET is the upper-case
               folder name and the series symbol, as the store spells it (EURUSD;
               SOLUSDT.P, Binance's perpetual); the format decides the venue. The PLAN is printed
               first, always: the server's directory, what is in it, which days are
               already held (by this lane or by `fetch`), refused, too recent, or met
               by an earlier fetch's key, and how many it would import. Then it asks
               for `import N days` at a terminal — or --yes; with neither it is
               REFUSED — and sends ONE request per calendar month, a line on stderr as
               each finishes. --dry-run stops at the plan; --dry-run --verify decodes
               every importable file, month by month, and writes nothing. Interrupted,
               it resumes: the same command finds the finished days held. It exits 1
               when a day is refused for what is in its file (each is listed, and no
               commit key is spent for it) and when the directory is absent or
               unreadable — the plan then names the server's path and how to fill it.
               Needs this box's device key on that datahub's access list with the
               control role or higher

WRITE — drives the standalone `backtesthub` engine on THIS machine against a store on THIS
machine (the engine is attached beside this binary in a Linux release; on Windows it is
a binary you supply yourself, and the failure message says how) — see --engine below:
  fetch --source starter
               download the PUBLISHED starter dataset (real bars, plain HTTPS, no venue
               and no credentials) and load it. For a box a venue cannot be reached from
               — a geoblock, a locked-down network. Verified against its published
               SHA256SUMS; safe to re-run. Takes no SPEC and no window: the published
               span is fixed. (There is no demo or synthetic tape: a fresh store holds
               nothing until real data is loaded — this, a venue fetch, or `import`.)

EXPORT — one series OUT to a standalone file (--out FILE). It READS through a datahub
like every READ verb below: there is no local-store route, and --store is refused on it
by name (decision 0084). For files on this machine, serve them with a datahub first:
vike-backend datahub --store DIR. TWO ROUTES, chosen by --addr:
  export SPEC  without --addr — the ENGINE writes BARS to Parquet, reading them from the
                 datahub ITS settings name: the config.datahub_addr row
                 (default {datahub}). Optionally bounded by --from/--to INDEPENDENTLY (an export slices what
                 the store already holds, so one bound alone is meaningful and neither
                 is required). Any venue the store holds
               with --addr — THIS binary walks that datahub and writes the rows itself:
                 --format jsonl|csv, --kind bar|quote|trade|book|depth (SPEC is VENUE:SYMBOL
                 for the tick and book lanes, which have no interval; a book row is one
                 recorded event, as recorded, or with --levels N the best N levels a side
                 of the book after it), BOTH --from and --to required.
                 The range is walked in --window steps so each request fits one frame;
                 nothing is held in memory but the window being written. `--format
                 parquet` is the ENGINE route's — the wire carries rows, never a file.
                 --kind quote --lane LANE reads a `source=` lane (`ls` lists it): the
                 recorder's tape of a venue that also has an archive, which nothing else
                 reads. --kind bar --lane LANE reads a bar lane: bars built from
                 aggregated trades, which are not the exchange's candles

READ — asks a running vike-datahub over RPC about the store THAT process opened. There
is no local-store read anywhere (decision 0084): for files on this machine, serve them
with a datahub first — vike-backend datahub --store DIR. Every datahub, that one too,
admits only the device keys on its project's access list (`vike-cli backend mint-missing`
registers this box's own):
  get SPEC     the ROWS themselves — the only verb here that shows you a PRICE rather
               than a fact about a store. SPEC is VENUE:SYMBOL:INTERVAL and it reads
               BARS; there is no --kind, because a quote, a print and a book level are
               three other row shapes. A WINDOW IS REQUIRED — --days N, or --from/--to,
               either of which stands alone — and at most --limit rows are printed
               (default 1000). Hitting that ceiling is REPORTED, never silent. --format
               jsonl is the pipeline form; bulk extraction is `export`. --lane LANE
               reads a bar lane (`ls` lists it) instead of the venue's candles
  ls          every stored series with its coverage — kind, venue, symbol-or-group,
               interval, rows, days, first/last. Add --asset-class for the asset class each
               instrument's venue actually RECORDED
  gaps         the same enumeration, answering the OTHER question: the HOLES inside
               each matched series' recorded span, in epoch-ms. Takes the same
               --kind/--venue/--name filters — and costs one extra round trip per
               MATCHED series, so filter first. Every matched series is printed, clean
               ones included: an empty listing would otherwise mean either `nothing
               matched` or `nothing is missing`, which are opposite answers
  coverage     the CROSS-KIND report: per instrument, which days have trade but no
               book. A half-failed recording is invisible per series and obvious here
  health       the STRUCTURAL scan: series whose OWN CATALOG contradicts itself — more
               bars than the span's grid can hold (a duplicated timestamp, proven by
               counting), a span that ends before it begins, more date= partitions than
               days. `gaps` finds what is MISSING; a gap shows up in an equity
               curve as a flat stretch, while a duplicated bar shows up as alpha. Only
               the offenders are printed; the summary says how many were scanned.
               ⚠ NOT a row scan — it reads no OHLC, and --help's own note says why
  universe     point-in-time MEMBERSHIP: what the store CONTAINED over a window, with
               each instrument's FIRST and LAST recorded row. The survivorship-bias
               defence — ask `ls` what exists today and a backtest of last year
               samples only the instruments that survived it. Bound it with
               --from/--to (both optional and independent). ⚠ The endpoints are
               EVIDENCE of a listing/delisting, never a venue calendar: a tape that
               stops may be a delisting, a dead recorder, or an unfinished backfill
  gate SPEC    IS THE STORE READY? The verb whose PRODUCT is the exit code, for a CI
               step or an ExecStartPre= that must not run an hour of backtest on a
               store missing three weeks in the middle. SPEC is VENUE:SYMBOL[:INTERVAL]
               (or VENUE:@GROUP) and SELECTS among the series a datahub reported —
               exactly, never a substring. --require-days N is REQUIRED; add --max-gap D
               for the other half of `a whole year, WHOLE`, and --require-kind K for the
               tape a run needs. The verdict is a DOCUMENT naming every criterion that
               passed and failed, on stdout under both formats; the exit code is its
               summary — 0 held, 6 a declared threshold BREACHED (the command WORKED),
               7 nothing was evaluated

DELETE — reaches EITHER store: --addr asks a datahub, otherwise the engine runs
against a store on this machine:
  rm           DELETE stored series, IRREVERSIBLY. Selects on the four series
               dimensions — --kind and --venue are REQUIRED, and an omitted
               --symbol/--group/--interval is a wildcard. The PLAN is printed first,
               always: the store that answered, then every matched series with its
               rows/days and the commit keys that wrote it. --produced-by asserts
               that EVERY key of EVERY matched series carries that prefix, and one
               foreign key refuses the whole run; it is REQUIRED for a sweep. Confirm
               with --yes, or by typing `delete N series` at a terminal. Matching
               nothing is a SUCCESS. There is no --force. A `source=` lane (the recorder's
               tape of a venue that also has an archive; `ls` lists it) is reached only
               by naming it, --lane LANE, and the series beside it is then left alone

REPAIR — drives the engine against a store on THIS machine; there is no --addr:
  repair       REBUILD one series' index from its parts — the repair the store's own
               `manifest … is missing` error names. Selects ONE series EXACTLY
               (--symbol or --group is REQUIRED, and --interval too on `bar`), because
               a series whose base manifest was deleted appears in no listing, so a
               wildcard could not reach the very failure this fixes. REHEARSES BY
               DEFAULT: it prints what the rebuild would recover and what it would
               LOSE, writes nothing, takes no lock, exits 0. --yes performs it;
               --dry-run wins over --yes. A rebuild that recovers the index but not the
               idempotency log exits NON-ZERO and says what to do. It REFUSES while
               another writer holds the series lock, and never waits for one

RENAME — drives the engine against a store on THIS machine; there is no --addr:
  rename       MOVE an instrument's series under its new name: every per-symbol series
               of --venue stored under --symbol goes under --to (every kind, interval and
               source= lane unless --kind/--interval/--lane narrow it), merging into a
               series already there. Every row, part and commit key is kept, and a row
               that carries its own symbol reads the new one. Prints the PLAN first
               (MOVE or MERGE per series); confirm with --yes, or by typing `rename N
               series` at a terminal; --dry-run stops after the plan. Run it AFTER the
               deploy: it holds the old series' lock while it moves. A re-run finishes an
               interrupted one, and matching nothing is a SUCCESS that says so


STORE — `--store DIR` names the hist-store root for the verbs that WRITE one: `fetch --source
starter`, `rm`, `repair` and `rename`. Without it they act on the store the datahub serves: the
`config.store_root` row, else this project's own folder. It is REFUSED by name on `export` and on
every READ verb: history is read through a datahub (decision 0084). The flag is left out of the
option table so that a bare `--store` still meets that refusal rather than a complaint about a
missing value.";
