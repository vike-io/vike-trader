//! `vike-cli data`'s usage roster: what `--help` prints, and what the MCP text is held to.
/// The command's own usage roster. `pub(crate)` so `crate::cmd::mcp`'s
/// `the_instructions_name_only_real_commands` can hold the MCP `instructions` text to the
/// subcommands and flags THIS module actually accepts, rather than to a copy of them.
pub(crate) const USAGE: &str = "\
usage: vike-cli data <group> <verb> [options]

Get market data into the hist store the backtest engine reads, and ask a running
vike-datahub what is already in one. The two halves reach DIFFERENT stores and their
flags do not mix — each half's flags are refused on the other, by name.

GROUPS — every verb below lives in one, and the group is REQUIRED: there is no bare
`vike-cli data <verb>`, and each pre-group spelling is refused by name with its
replacement rather than silently accepted:
  hist         WHEN = a past window. The verbs on THIS page
  realtime     WHEN = now (watch | status | record) — `data realtime --help`. `record`
               is a SUB-GROUP of its own (ls | add | rm): what the BOX persists, as
               rows in this project's settings database. Its --addr half — editing
               ANOTHER box's rows — is designed and refused by name
  catalog      what is addressable (ls | show | refresh | venues) — `data catalog --help`
  source       where rows may come from (ls | show) — `data source --help`, and it
               reaches no server: every answer says what it did NOT check

FETCH — asks a DATAHUB, which is the only thing that reaches a venue:
  fetch SPEC   ask the datahub to pull REAL bars into its store. SPEC is
               VENUE:SYMBOL:INTERVAL (e.g. binance:BTCUSDT:1h). Needs a window:
               --days, or --from/--to. Most venues need no credentials — it is public
               market data — and a venue that needs one is marked: `vike-cli data source
               show VENUE` says what a venue needs and how far back each of its doors goes.
               VENUE:SYMBOL:funding fetches a perpetual's funding-rate series (a binance
               perpetual is SYMBOL.P, e.g. binance:BTCUSDT.P:funding).
               ⚠ Needs a reachable datahub (--addr, default 127.0.0.1:7878): history
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
               runs to its end. On a datahub that holds node keys `cancel` needs the
               Control key and `running` the Observe one; a key-less loopback datahub
               serves both. Against a datahub older than these verbs both are refused
               by name with nothing sent — restarting it is then the only stop

IMPORT — asks a DATAHUB to read a vendor ARCHIVE you downloaded yourself, from a folder on
the DATAHUB's OWN box, into its store. No vendor credential reaches vike: the download is
yours, under your own account:
  import FORMAT DATASET
               read ONE dataset of an archive FORMAT the datahub advertises — today
               dukascopy-bi5, Dukascopy's daily tick files, which you sync from the
               vendor's bucket under your own AWS account — from
               <project>/market_data/imports/FORMAT/DATASET/ ON THE DATAHUB'S BOX.
               It stores the ticks and derives bars per imported day (--bars, default
               1m). DATASET is the vendor's upper-case folder name (EURUSD) and becomes
               the series symbol; the format decides the venue. The PLAN is printed
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
               Needs the Control key on a datahub that holds node keys; a key-less
               loopback datahub serves it

WRITE — drives the standalone `backtest` engine on THIS machine against a store on THIS
machine (the engine is attached beside this binary in a Linux release; on Windows it is
a binary you supply yourself, and the failure message says how) — see --engine below:
  fetch --source starter
               download the PUBLISHED starter dataset (real bars, plain HTTPS, no venue
               and no credentials) and load it. For a box a venue cannot be reached from
               — a geoblock, a locked-down network. Verified against its published
               SHA256SUMS; safe to re-run. Takes no SPEC and no window: the published
               span is fixed
  fetch --source demo
               write the SYNTHETIC demo tape into the store. Venue `demo`, a closed-form
               curve, NOT market data — it is the slice the shipped
               user_data/profiles/backtest.toml names, so a fresh install can run that
               profile immediately. Safe to re-run. Takes no SPEC and no window

EXPORT — one series OUT to a standalone file (--out FILE). It READS through a datahub
like every READ verb below: there is no local-store route, and --store is refused on it
by name (decision 0084). For files on this machine, serve them with a key-less datahub
first: vike-backend datahub --store DIR. TWO ROUTES, chosen by --addr:
  export SPEC  without --addr — the ENGINE writes BARS to Parquet, reading them from the
                 datahub ITS settings name: the config.datahub_addr row
                 (default 127.0.0.1:7878). Optionally bounded by --from/--to INDEPENDENTLY (an export slices what
                 the store already holds, so one bound alone is meaningful and neither
                 is required). Any venue the store holds, `demo` included
               with --addr — THIS binary walks that datahub and writes the rows itself:
                 --format jsonl|csv, --kind bar|quote|trade (SPEC is VENUE:SYMBOL for the
                 two tick lanes, which have no interval), BOTH --from and --to required.
                 The range is walked in --window steps so each request fits one frame;
                 nothing is held in memory but the window being written. `--format
                 parquet` is the ENGINE route's — the wire carries rows, never a file

READ — asks a running vike-datahub over RPC about the store THAT process opened. There
is no local-store read anywhere (decision 0084): for files on this machine, serve them
with a key-less datahub first — vike-backend datahub --store DIR:
  get SPEC     the ROWS themselves — the only verb here that shows you a PRICE rather
               than a fact about a store. SPEC is VENUE:SYMBOL:INTERVAL and it reads
               BARS; there is no --kind, because a quote, a print and a book level are
               three other row shapes. A WINDOW IS REQUIRED — --days N, or --from/--to,
               either of which stands alone — and at most --limit rows are printed
               (default 1000). Hitting that ceiling is REPORTED, never silent. --format
               jsonl is the pipeline form; bulk extraction is `export`
  ls           every stored series with its coverage — kind, venue, symbol-or-group,
               interval, rows, days, first/last. Add --class for the asset class each
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
               nothing is a SUCCESS. There is no --force

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

options:
  --source SRC    fetch: WHERE the rows come from. Omit it for a VENUE (the default, and the
                  only source that takes a SPEC and a window), or name `starter` / `demo`,
                  each a fixed span that takes neither. Refused on every other verb by
                  name: they work on the store that is already there
  --days N        fetch: a window counting back from now. Refused on `export`, by name:
                  a day count back from NOW bounds a fetch, and an export slices what
                  the store already holds
  --from LABEL    fetch/export/universe: window start. TWO spellings, by which process
                  parses it. `fetch`, `universe` and a REMOTE `export` (--addr) parse it
                  HERE: epoch-ms, or a UTC date YYYY-MM-DD such as 2024-01-01 (that
                  day's midnight UTC) — an hour label is refused. An ENGINE `export`
                  hands it to the engine, which takes epoch-ms or a UTC hour
                  YYYY-MM-DDTHH such as 2024-01-01T00 — and refuses a bare date. On
                  `fetch` it needs a matching --to; on an ENGINE `export` and on
                  `universe` it stands alone; on a REMOTE `export` BOTH are required,
                  because a windowed walk has to know where the first step begins and
                  where to stop. ⚠ Parsed HERE means an unreadable label is a usage error
                  rather than the engine's. On `import` it is the first DAY, inclusive:
                  YYYY-MM-DD (or the epoch-ms of a UTC midnight), naming a day that
                  exists — an instant inside a day is refused. It stands alone there,
                  and an omitted one is the dataset's own first day
  --to LABEL      fetch/export/universe/import: window end, same spellings and the same
                  asymmetries. On `universe` both bounds are INCLUSIVE, and an omitted
                  one takes the store's own endpoint rather than the wall clock. On
                  `import` it is the last DAY, INCLUSIVE — --from 2024-01-15 --to
                  2024-01-15 imports that one day, whole, where a fetch's --to is an
                  instant — and an omitted one is the dataset's own last day
  --bars IV[,IV...]
                  import: the bar intervals derived per imported day from its ticks,
                  each one dividing a UTC day (1m, 5m, 1h, 1d), at most 4. Default 1m;
                  `--bars none` stores the ticks alone. A step that does not divide a
                  day (7m) is refused here: `data hist fetch` derives it later, reading
                  an imported day from the stored ticks rather than downloading it
  --out FILE      export: the file to write — Parquet on the ENGINE route, the rows in
                  --format on the remote one. REQUIRED there, refused elsewhere
  --window SPAN   export --addr: the wall-clock width of one step of the walk (4h, 1d,
                  7d). Default 30d for bars, 1d for the tick lanes. Each step is ONE
                  request answering in ONE frame, so lower it when a window overruns the
                  64 MiB frame cap and raise it for fewer round trips. Refused
                  everywhere else, including the ENGINE export, which walks nothing
  --store DIR     the WRITE half, rm and repair — the verbs that WRITE a store: the
                  hist-store root to act on. REFUSED by name on `export` and on every
                  READ verb: history is read through a datahub (decision 0084)
  --engine PATH   the WRITE half, export without --addr, rm and repair: the standalone
                  engine to run, instead of searching <project>/bin, this executable's
                  directory, and PATH
  --addr H:P      fetch, running, cancel, import, every READ verb, rm and export: the
                  datahub to ask (default 127.0.0.1:7878). On `import` the archive is
                  read on THAT datahub's box, which is where its files must be.
                  It binds localhost, so reach a remote one over `ssh -L 7878:localhost:7878`.
                  ⚠ On `rm` and `export` it is the REMOTE ROUTE — a different grammar,
                  not a different address — and naming it there excludes --engine (and
                  on `rm`, --store). The ENGINE export reads the datahub the engine's
                  own settings name (the config.datahub_addr row), not this flag.
                  On `rm` it is served only by a datahub that holds node keys: a key-less
                  one serves no delete verb at all. REFUSED
                  on `repair`, by name: a datahub can only reach series it ENUMERATED
  --kind K        ls/gaps/health/universe: keep series whose kind contains K
                  (bar/quote/trade/book/depth). On `universe` it narrows WHICH universe
                  — `--kind bar` is the set a bar-driven profile actually reads. Refused
                  on `coverage`, whose row IS the join across kinds.
                  rm/repair: the EXACT kind (required)
                  export --addr: the EXACT row shape — bar | quote | trade, the three
                  the wire can read. Refused on an ENGINE export, which writes bars
  --venue V       every READ verb: keep rows whose venue contains V. rm/repair: the
                  EXACT venue (required)
  --symbol S      rm: the exact symbol of a PER-SYMBOL series, omit to wildcard.
                  repair: the same, but REQUIRED (--group is its alternative)
  --group G       rm/repair: the exact group of a GROUPED series (which has no symbol at
                  all). An alternative to --symbol, never a pair
  --interval I    rm: the exact bar interval, omit to wildcard. repair: the same, but
                  REQUIRED on `bar`. Refused with --group on both
  --produced-by P rm: the commit-key PREFIX every key of every matched series must
                  carry. REQUIRED whenever the selector can match more than one series.
                  ⚠ A repo-relative PRODUCER PATH resolves to its prefix on the LOCAL
                  route (--store) only — the datahub resolves nothing, so a path is
                  refused by name under --addr rather than asserted literally
  --dry-run       rm/repair/import: print the plan and stop. Wins over --yes. On `repair`
                  it spells the DEFAULT — that verb rehearses unless told otherwise. On
                  `import` nothing is decoded and nothing is written
  --verify        import, beside --dry-run only: after the plan, DECODE every importable
                  file — one request per calendar month — and check it as an import
                  would, writing nothing. It lists the days an import would refuse
  --yes           rm/import: the non-interactive confirmation. Without it and without a
                  terminal, the run is REFUSED — never read from a pipe. repair: what
                  turns the rehearsal into a write; without it nothing is written and
                  the run says so and exits 0
  --name N        every READ verb: keep rows whose NAME contains N — the symbol of a
                  per-symbol series, or the GROUP of a grouped one (a grouped series
                  has no symbol at all, which is why this is not called --symbol)
  --class         ls: also show the ASSET CLASS each instrument's venue recorded in
                  the store's kind=properties tape — the LATEST one on record. One
                  extra round trip per distinct (venue, symbol), so filter first. The
                  cell says which kind of answer it is: the class word itself, or
                  `unclassified` (a grid was recorded and it named no class — the venue
                  producer is not wired), `no-properties` (nothing recorded for this
                  instrument at all), `(group)` (a grouped series' name is a GROUP, not
                  a symbol, so nothing was asked) or `(error)`, whose reason is printed
                  under the row
  --partial-only  coverage: keep only instruments that have a day some recorded kind
                  covers and another does not
  --require-days N
                  gate: the recorded SPAN each selected series must reach, in whole
                  days. REQUIRED there — a gate with no criterion exits 0 having checked
                  nothing, which is the one answer a CI step must never get. ⚠ It is the
                  SPAN, not the days actually in it: a year-wide series missing three
                  weeks in the middle passes this and fails --max-gap
  --max-gap D     gate: the largest HOLE each selected series may carry — a duration in
                  this workspace's own grammar (4h | 1d | 2w). ⚠ A hole here is a WHOLE
                  UTC DAY (the store derives them from the date= partition set), so a
                  tolerance under 24h means `no missing day at all` and the verdict says
                  so. Omit it and the holes are NOT checked, which the verdict also says
  --require-kind K
                  gate: a kind the store must hold for this spec, repeatable. Defaults
                  to `bar`. It declares what the gate is ABOUT: a kind named here is
                  judged for days and gaps, one that is not is evidence for nothing but
                  the listing — so a one-row properties grid cannot redden an
                  instrument. A required kind the store lacks is a BREACH, never a
                  `nothing was evaluated`: you said it was required
  --limit N       get: how many rows to PRINT, at most 1000 — the default IS that ceiling,
                  and a larger --limit is refused rather than clamped, because a silent
                  clamp would leave you believing you had received what you asked for.
                  Hitting it is reported with the exact number of rows withheld. Refused
                  on every other verb by name: nothing else here emits rows
  --format F      HOW the answer is rendered: `table` (the default) or `json`. The uniform
                  output axis, on every verb. `--json` is its shorthand and the two are
                  refused together only when they DISAGREE. `jsonl` is `get`'s third form
                  — one JSON object per row, and stdout carries nothing else under it; on
                  the catalog verbs it is refused by name, because a catalog is not rows.
                  ⚠ On `export` this flag names a DIFFERENT AXIS — the FILE that --out
                  receives, which is section 7's own grammar: `parquet` on the local
                  route, `jsonl`|`csv` on the remote one (--addr), and a terminal
                  rendering (`table`/`json`) is refused by name there with that
                  correction. The report `export` prints about what it wrote is `--json`,
                  which is unchanged
  --json          shorthand for --format json: one JSON object on stdout describing the run. For the ENGINE routes
                  (fetch/export): what was asked for, which
                  engine ran, and its own report lines verbatim, with that report moved
                  to stderr so stdout is the document and nothing else. For get: the
                  request, the resolved window, the BARS, and `returned` beside `shown`
                  so a consumer can tell a complete answer from one the ceiling cut. For
                  ls/gaps/coverage: the rows, carrying each series' raw symbol AND group
                  rather than this side's rendering of them, plus — under --class —
                  an asset_class_status naming WHICH answer each row got, so an
                  absent class can never be read as a present-but-empty one. For
                  health/universe: every row INCLUDING the healthy and absent ones,
                  each carrying the numbers its verdict was derived FROM, so a consumer
                  can re-derive it rather than trust it. For gate: the same verdict the
                  table renders — every criterion with its verdict, plus each selected
                  series' own numbers, so a consumer re-derives the judgement instead of
                  trusting it. ⚠ It is emitted on a BREACH too, unlike every other
                  document here: a breach means the command WORKED, so the verdict is
                  the answer rather than a failure. For running: every running fetch
                  with every field the datahub sent and its pasteable spec. For cancel:
                  the requests it flagged and the ones it could not stop, the same
                  fields. For import: the datahub's plan exactly as it sent it, one
                  object per month request carrying the datahub's own outcome, and a
                  summary with BOTH importable counts — the preview's and the
                  datahub's month by month — and every refused day; the plan, the
                  progress and the summary lines go to stderr. For rm: the plan, the
                  outcome, and the provenance refusal when there is one. For repair:
                  the plan with every RebuildReport count, the LOSSY verdict, and
                  whether anything was written
  -h, --help      this message";
