# Changelog

All notable changes to vike_trader_rust are recorded here.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/); versioning is
[SemVer](https://semver.org/) — pre-1.0, a minor bump may include breaking changes.

Per-release notes are also auto-generated on GitHub (the PRs merged since the previous tag)
by `.github/workflows/release.yml`; this file is the curated human summary. Cut a release with
`just release <X.Y.Z>`.

## [Unreleased]

## [0.1.30] - 2026-09-21

### Added
- **`vike-cli secrets set` writes every key this workspace reads, not just the enumerable grid.**
  On a migrated box (`docs/decisions/0054`) the credential FILES stopped being read at all, and
  `set` refused every name outside the fixed `{VENUE}_{TIER}_{SUFFIX}` grid while advising an
  EDITOR — a route that edits a draft nobody loads. **Half the store had no writer anywhere**: an
  operator could see a credential in `secrets list` and rotate it nowhere. Three admission rules
  replace the grid-only test:
  - the **grid**, unchanged;
  - a name the settings registry proves a reader for **and** the credential classifier can PLACE —
    which is what admits the bespoke per-bridge keys (the FX logins, the per-venue server/host
    keys, the prediction-market proxy trio). ⚠ Both halves are load-bearing: the registry alone
    admits a store PATH read out of the process-env sweep, which is not a credential and would be
    the dead line the refusal exists to prevent;
  - **rotation** — any name this box's store ALREADY HOLDS. This is what reaches the deployment's
    own secrets (the Telegram trio, a Cloudflare token) without a hand-kept roster that would rot,
    and it cannot invent a dead line, because the row is already there.
- **A LABELLED account (`KEY__LABEL`) is written rather than refused**, when its base is a grid key.
  ⚠ The hazard the old refusal was written about does NOT go with it — it was the suggestion list
  offering the UNLABELLED base, which an operator then set, overwriting the DEFAULT account's
  signing key. Writing the typed name is what ends the temptation; the base is left byte-for-byte.
- **`vike-trader-ci-deploy verify-tools` — the deploy now proves the runtime tools it placed.** It
  walks the tool table, compares each destination against the release manifest the deploy itself
  verified, and FAILS the deploy run on drift or absence.

### Fixed
- **A stale deploy helper stopped being silent.** MEASURED on the CI box: the installed helper predated
  the runtime-tool table, so for eight days every deploy installed the two binaries, placed **no
  tool**, and reported success — the JForex sidecar jar three commits stale (both Dukascopy demo
  logins timing out at 240s) and no LightGBM on the box at all, with nothing red anywhere. The
  helper is installed BY HAND and nothing deploys it; the new check catches exactly that, and the
  mechanism is the verb's own ABSENCE, since an old copy dies on its usage arm.
- **`scripts/fetch_release_tools.sh` survives a repository RENAME.** It derives its slug from the
  checkout's `origin`, which a rename does not update, and GitHub answers the old path with a 301 —
  so the API read came back empty and the run died with `no release 'latest' … : Moved Permanently`,
  a message about a TAG for a problem with the NAME.
- **`secrets move-venue-config` stopped colliding with itself.** Its collision check was fed the
  whole live credential set, including the rows being moved, so every rendered name matched its own
  source and the verb refused ALL TEN keys — a refusal nothing could satisfy. It now evaluates
  against the rows that stay.

### Changed
- **The pmxt Polymarket-L2 archive is OBSOLETE — it stopped publishing on 2026-08-10.** The last
  object is `polymarket_orderbook_2026-08-10T00.parquet`; everything after is 404, and the
  attributed site refuses connections. ⚠ The collector never noticed: a 404 is mapped to "the hour
  hasn't been published yet, skip it", which is correct for an archive on a lag and wrong for one
  that has ended — so a run over any later range skips every hour and **exits 0 having ingested
  nothing**. The module is kept (the ingested history is real and its mapper is the reference for
  that Parquet schema); what is dead is the SOURCE.
- **`ureq` is held at 3.4.0 and the pin carries the measurement.** 3.4.1 turns `timeout_recv_body`
  from a per-READ idle bound into a TOTAL body ceiling, which is not what
  `vike-backfill`'s HTTP module documents it as. Measured A/B on one test: 3.4.0 passes at ~5.0s,
  3.4.1 fails at 3.01s, three for three. ⚠ The gate that caught it is the `coverage` workflow, not
  the CI fast lane — `vike-backfill` is excluded from the roster, so a green `ci` says nothing here.

## [0.1.29] - 2026-09-21

### Added
- **`vike-cli secrets move-venue-config [--dry-run]` — ruling 10's move, as an operator act.** Ten
  config-shaped keys that were never credentials (an IBKR gateway host, an FXCM connection name, a
  JForex server, the polymarket proxy trio) leave the credential store and become
  `config.venue.<venue>[.<tier>].<field>` settings rows, which `config show` renders and
  `config set` writes.
  - ⚠ **An operator act, not a step inside `secrets migrate`**, for the reason `config adopt` is
    one and a sharper one: it DELETES rows from the only copy of a box's venue keys. `--dry-run`
    answers the whole verdict and writes nothing.
  - ⚠ **Two refusals, both writing NOTHING**: a collision (a rendered name a live `credential` row
    already holds — SQLite cannot constrain a name across two tables) and a divergence (two names
    collapsing onto one key while holding DIFFERENT values, which would hand one dukascopy account
    the other's JForex server).
  - ⚠ **Every reader keeps finding the legacy name it looks up.** The credential map folds the
    settings rows back in, at BOTH read paths — the inner read every composition root reaches, and
    the SCOPED read `polymarket`'s egress uses directly. Without the second, the proxy family would
    have been the one thing the move broke while every other reader kept working.

### Internal
- `ScopedSecrets::fold_in` — the seam that makes the scoped fold possible without rebuilding the
  type from a map, which would have dropped its findings (the permission warning, the shadowed
  file, the source) in silence. It answers three states rather than a `bool`, because a caller must
  tell a COLLISION — a half-done migration, to report — from NOT DECLARED, which is the scope doing
  its job: folding in a name nobody asked for would widen a read that was deliberately narrowed.

## [0.1.28] - 2026-09-21

### Added
- **A venue can be asked WHICH ACCOUNT a key trades, and the answer is recorded.** `vike-mount`'s
  `book_identity` table said, for binance/bybit/okx/deribit, that the credential store names no
  account and only an authenticated call could — and there was no such call. There is now:
  `vike_exec::recon::ReconClient::fetch_account_identity`, implemented for binance (the `uid` rides
  `/api/v3/account`, a body the fee read already pulls, so it costs that venue NOTHING), okx (one
  signed `GET /api/v5/account/config`) and deribit (one `private/get_account_summary` with
  `extended: true`). The mount folds the answer through the same `vike_model::account_confirmation`
  handshake dukascopy and hyperliquid already use, and `vike-cli secrets confirm` files it into
  `account.venue_account_id`.
  - ⚠ **It runs at TWO sites and neither covers the other**: the startup preflight, for every ARMED
    venue whether or not it is mounted, and `make_engine_for_account`, for every MOUNTED venue.
    Measured on the deployed box: 16 `account` rows across 13 venues against a profile that mounts
    ONE. A mount-only rung would have recorded one venue's book while the store asked about
    thirteen; a preflight-only one would never reach deribit, which builds its client inline.
  - ⚠ **The identity read is ordered AFTER the fee read, and that ordering is load-bearing.** On
    binance spot both ride `/api/v3/account`; asking first turns one signed request into two.
  - ⚠ **bybit is deliberately NOT implemented.** Its demo key answered `retCode 33004` (*Your api
    key has expired*) when the probe went to measure what its response names, so nothing was
    measured and nothing was written. Filling that row from the venue's documentation instead is
    the `canWithdraw` trap this workstream exists not to take.

### Changed
- **The three `vike-tradehub` node scopes say what they GRANT**: `Observe`/`Control`/`Admin` become
  `Read`/`Write`/`Account`. ⚠ **The variant names ARE the wire** — `Request::Auth` carries `Scope`
  through derived serde — so this is a protocol break between a node and a client of different
  vintages, not a rename. `Scope` remains NOT a ladder: `Account` is not a superset of `Write`.

### Fixed
- **The paper-mount warning stopped telling an operator two false things.** It read *"no DEMO
  credentials for it were found in the settings store … add BYBIT_DEMO_API_KEY + _API_SECRET to
  `<project>/settings/secrets.env`"* — on a box where the credentials ARE in the store (the startup
  preflight had demoted the venue over an expired key, and said so one screen earlier) and where
  `secrets.env` is NO LONGER READ because the settings database answers. It now states the outcome,
  names BOTH causes, points at the preflight line that separates them, and names the VERB rather
  than an artifact: `vike-cli secrets set` writes whichever store answers, `vike-cli secrets path`
  prints which one that is. Four sites carried the hardcoded path; they now share one helper.

### Security
- **The capture sanitizer redacts the PARENT account and the human behind it, before anything
  captured them.** `REDACT_KEYS` gained `parentUid`/`mainUid`/`masterUid`/`subAccount` and their
  spellings — the field that separates a sub-account from its master, which is precisely what the
  blind-CEX probes went looking for — and then `email`/`username`/`system_name`, because a deribit
  `get_account_summary` was MEASURED to answer all three while `redacted_paths` came back EMPTY.
  - ⚠ **One residual cannot be closed and is PINNED rather than left implied**: deribit's account id
    is spelled `id`, and `id` may never join that list — it is every order id, trade id and combo
    leg, and redaction is by KEY, not by PATH. A test fails a later author who "fixes" it and says
    what it would cost.

### Internal
- **Ruling 10's move gets all three of its prerequisites, and moves nothing.**
  `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §12 forbids the ten config-shaped
  keys leaving `credential` until §6.2's ordering (A) exists. It does now: a name renderer
  (`venue_setting_names`, one row to possibly SEVERAL legacy names — the ten names are nine rows
  because the dukascopy pair was measured equal), the collision check §6.2 says the migration owes,
  the read-side fold that keeps every loader finding the name it looks up, and `move_pending_rows`
  itself. No verb calls it yet.
  - ⚠ **The values go to `setting` rows, NOT to `venue_setting`** — owner's ruling. That table would
    have been a THIRD key shape for one question, and the spec's own open items say it arrives
    "with no gate of its own … outside [`CONSUMPTION`]'s vocabulary because its keys are not
    `section.key` paths". A `config.venue.<venue>[.<tier>].<field>` key costs nothing and is visible
    to `config show`, writable by `config set`, and covered by that gate.
  - ⚠ **Both refusals write NOTHING**: a collision (a rendered name a live credential row holds —
    SQLite cannot constrain a name across two tables) and a divergence (two names collapsing onto
    one key with DIFFERENT values, which would hand one dukascopy account the other's JForex
    server).
- Studio's Local backtest backend is deleted — every backtest goes through the compute daemon
  (0078); a study may ask for one backtest and `poly_mm_batch` becomes a user study (0076); the
  Bayesian sampler comes down to `vike-ml`; one offscreen rasterizer replaces two in the PNG
  harnesses; `vike-chart`'s libm edge is a DEV edge, as its own comment demanded.

## [0.1.27] - 2026-09-20

### Added
- **An account is addressable end to end — from the wire, through the mount, to the risk ceiling.**
  A strategy mount names the account it wants (`MountStrategy`, behind its own capability, so a
  client naming one refuses an old node rather than being silently routed elsewhere), an
  order-carrying command names its account, and `vike_model::account_keys` gains the wire spelling
  that admits the reserved word. ⚠ **A mount that names a venue and no account is now REFUSED where
  that venue runs more than one engine** — it used to pick one. A single-engine venue is unchanged.
- **`policy.accounts.<venue>.<label>.max_exposure` — the open-notional ceiling can be stated PER
  ACCOUNT**, not only per venue. It composes as a `min` with the box-wide figure, so adding one can
  only ever narrow. ⚠ It is enforced per ENGINE, and two accounts sharing one venue book therefore
  carry it separately — which is what the shared-book warning below exists to tell you.
- **`vike-cli venues` — a read-only screen that says WHY a venue is still on paper**, per account,
  in the same projection the mount selects with. It answers the question the boot banner could only
  hint at.
- **A venue's own handshake can now NAME the account it authenticated as, and the store can keep
  it.** Hyperliquid asks `userRole` at mount instead of assuming the key is the account —
  the assumption was false for every agent wallet, and a wrong address there answers
  `accountValue: "0.0"` rather than an error, so the mount looked FLAT and HEALTHY while reading
  somebody else's book. The answer is parked into the state directory and folded by
  `vike-cli secrets confirm`, exactly as dukascopy's already was.
- **`vike-cli backend admin-key`** mints the third node key, the one 0065's `Admin` scope needs.
  ⚠ Minting it ARMS NOTHING: the account verbs also need `config.toml`'s `tradehub_account_admin`
  (`loopback`, which the daemon CHECKS against its own bind at boot, or `contained`, which you
  assert) and a restart. Until then the node holds no account writer at all.
- **The node wire can write a book** (`AccountVerb::SetBook`) — the twin of
  `vike-cli secrets set-book`, so `account.venue_account_id` is settable from a desktop and not only
  from a shell on the daemon's box. Repointing a row that already names a different book takes the
  typed confirm; writing onto an empty column does not.
- **A mount says WHAT IT TRADES to somebody who can read it**, over the wire.
- **The store records whether a series is spot or perp** (0061 step 2), five more bridges name their
  instrument's kind, and `vike-cli data list --class` reads it back. An options chain names its
  underlying's kind rather than an "asset class".

### Changed
- **The shared-book report is capped and says the thing no individual pair can.** N accounts on one
  wallet is N(N-1)/2 findings — at fifty accounts that is a flood in which the fact worth knowing
  (that fifty of them are ONE wallet) appears in no line.
- **The book a mount reports is the RECORDED one first, and only then a derived one.** Until now the
  answer came solely from the credential store, so the five venues whose keys name no account could
  never report a shared book however much the store had been told — `secrets set-book` wrote a column
  that changed no decision anywhere.
- **The compute plane stops reaching a venue.** `vike-cli data fetch` asks a datahub, the backtest
  path no longer dials a venue, and the spot collector retires. **No tool in this tree reaches
  ClickHouse for data any more** — the last direct reader is gone.
- **The asset-class taxonomy moved below the store** (vike-catalog → vike-model), and the `.P`
  suffix question gained ONE authority which the bridges ask OF THE VENUE rather than deciding
  locally.
- **The Polymarket takers left the simulator and the engine gates came home.**
- **The leaf libraries can be published to crates.io** (0071) — 0037's scope error, measured.
- **The boot banner names the credential store that actually ANSWERS**, rather than the one a path
  suggests. On a migrated box those are different files.
- **The layer graph straightens: the store comes down below the engine, and the recorder comes off
  it** — acquisition is a rank of its own rather than a leftover. The workaround binary that
  existed only to route around the old edge goes with its cause.
- **Eleven copies of the libm walk became one** (0074), hosted in `vike-model`. ⚠ The WALK is
  shared; the gate over it is deliberately not — 0074 carries why, and a later correction records
  what the first shape of that record cost.
- **`LIVE_CAPABLE`'s permissive arm carries its argument** rather than resting on the reader to
  supply one.

### Fixed
- **The backtest unit could not write a single run — the grant was one directory short.**
- **A release binary reported itself `dirty`** because the pinned tool archives landed in the
  checkout.
- **An FXCM login is not a book, and the account is not even fixed for the session.** The shim
  re-picks the first eligible account on every FFI call, so order flow moves to another account the
  moment the first enters a margin call. Recorded rather than patched — no CI runner stages that
  SDK — and the identity table stops asserting a book it cannot know.
- **A written `HYPERLIQUID_{TIER}_ACCOUNT_ADDRESS` is now audited against the venue.** It was the
  one input in that venue's path nothing checked, and the best-configured account was the only one
  never verified. The mount still uses what you wrote — a disagreement reports and re-routes
  nothing.
- **A parked confirmation can name a LABELLED account.** The address is a credential-key prefix, and
  a labelled account has none, so such a record could never be folded.
- **A symbol becomes a directory name, so it may not contain a path separator** — and the catalog
  renders the core symbol without one, saying so when it cannot.
- **An UNREADABLE account epoch is not a CHANGED one**, on the MCP confirm path.
- **The recorder's family GLOB is not a directory name**, so the group name is rendered.
- **Studio's Remote backend dialled the daemon that refuses its verbs.** The compute verbs live on
  the data daemon; the trading node answers `required_scope` for them and always would have.
- **A flake diagnosed, a rotting number retired, and an annualisation rule made structural** rather
  than restated in prose that drifts.

## [0.1.26] - 2026-09-19

### Added
- **`vike-cli config mirror` writes the four settings files into the settings database** (0057
  phase 1). ⚠ **This is MIRRORED, not crossed: the FILES still win and nothing this box resolves
  changes.** The rows are a regenerable copy, which is what makes the phase safe to land on a live
  box. Nothing deletes a `.toml`, and 0057 is explicit about the order — *"the read-back path must
  land BEFORE the files are retired"* — so retirement waits on a later phase.
- **The `account` table gains a lifecycle, and the wire gains the barrier 0065 designed** — an
  account is a row, addressed by the venue's own number, and a daemon refuses an ambiguous one by
  name instead of guessing.
- **A venue catalog is fetched ON by default** (0066), with a baseline instrument list shipped so a
  box with no network still addresses the common symbols.
- **`vike-cli backtest --html`** and the batch-B flag doors on the backtest CLI.

### Changed
- **The settings store's `value` column holds a JSON scalar** rather than a TOML rendering.
  ⚠ **Run `vike-cli config mirror` before the new binary's unit restarts.** Every row both live
  boxes carry was measured identical across the change, so neither needs the re-run to keep
  booting; the ordering rule is what makes that true of the next box too.
- **The market-data vocabulary moved out of `vike-model`** into a crate of its own.
- **rhai 1.26.1**, and the licence comment stops saying proprietary.
- **The Data Manager's seven tabs and nested sidebar became one rail.**

### Fixed
- **A stale or wrongly-typed settings row degrades instead of bricking the box.** A row the new
  reader could not parse was a hard startup refusal that took down `vike-cli config mirror` — the
  one command the refusal told the operator to run — and `sqlite3` is installed on neither
  deployment box, so there was no way back. A row that cannot be read now DEGRADES by name: the
  files still answer, and every command keeps running.
- **`config.datahub_addr` reached the GUI and no CLI dialer at all**, so a box that set the key
  moved some of its dialers and silently left the rest on the compiled-in default.
- **The kline collectors refused a venue at the wrong seam** (0059 phase 1), so the one-shot
  backfill bins and the `backtest data fetch` verb disagreed with the supervisor about what is
  collectable. Four hand-copied venue rosters became one registry (0059 phase 3).
- **`vike-cli backtest --html` claimed nothing was recomputed while the monthly table recomputed.**

## [0.1.25] - 2026-09-15

### Added
- **The dukascopy mount addresses the ACCOUNT it is mounted for** — the last venue whose arm read
  one account's credentials whatever account it was asked to mount.
  `crates/vike-mount/src/lib.rs`'s `make_engine_for_account`
  had a `("dukascopy", _)` arm that hardcoded `DukascopyAccount::Demo1` and ignored its own
  `account` parameter, and `arm_addresses_accounts` refused the venue outright so nothing could ask
  it to do otherwise. `docs/superpowers/specs/2026-09-14-the-credential-schema.md` ruling 13 refused
  every KEY-GRAMMAR fix for that and said to wait for the database — *"once the account is a COLUMN,
  a name carries no account, no tier and no index"* — and §12 barred the list until the arm threaded
  `account` into an account-aware loader. The column, its reader and its key-name reader have all
  landed, so this is that work.
  - **The mapping is keyed on the ROW, and specifically on its credential-key OWNER PREFIX** —
    `DUKASCOPY_DEMO1_` or `DUKASCOPY_DEMO2_`, through the new
    `vike_dukascopy::DukascopyAccount::from_key_prefix` (the exact inverse of its new `key_prefix`).
    That is the store's OWN account identity — `vike_secrets::schema`'s `Classification::owner_prefix`
    says an account's owner prefix is recoverable from any one of its rows, which is why the
    migration needs no stored discriminator for the one pair sharing `(venue, tier, label)`. The
    ADDRESS an operator writes is `account.venue_account_id`, the venue's own number, matched first
    and then a `label` if a row carries one. ⚠ **No label is required, invented or read from a key
    name**: both dukascopy rows carry `NULL` and the owner refused the provisional `DEMO1`/`DEMO2`
    spellings twice.
  - ⚠ **`AccountLabel::Default` is `DukascopyAccount::Demo1` on every box, whatever the database
    holds.** Argued at `crates/vike-mount/src/dukascopy.rs`'s `resolve_account`: re-pointing it
    would move which LEGAL ENTITY an order reaches (Dukascopy Bank SA vs Dukascopy Europe IBS AS)
    with no operator act, and after the migration there are two unlabelled `(dukascopy, demo)` rows
    and nothing marks either "the default" — so the store has no better answer to give. A
    `Backend::Files` box, a database older than the `account` table, and a process whose root
    declared no project all behave exactly as before: the default account mounts, every labelled one
    is refused.
  - ⚠ **An account the store cannot identify is REFUSED to paper with a named `error!`** — never
    coerced onto Demo1. Five refusals, each naming what an operator can do: no `account` table, no
    such row, an ambiguous address, a row whose key names name no dukascopy family (or both), and
    the sidecar claim below. `vike_config::ArmingBlock::AccountNotInStore` is the new arming row, and
    the arming PROJECTION consults the same resolver as the mount so the two cannot disagree about
    what will arm.
  - ⚠ **ONE JForex sidecar per process, refused rather than reported.** Two concurrent sidecars are
    UNPROVEN and the risk falls on the account that already worked: `Bridge.java` sets no platform
    cache directory, so two JVMs of one user share the per-user default, and a corrupted cache is
    the INSTANT-`login failed`-every-time failure until the directory is deleted by hand. That is
    why this is not a `docs/decisions/0013` degrade — it would degrade a capability that already
    worked, not the one being added. What retires it is a measurement, named at that module: a
    per-sidecar cache directory, then two real concurrent logins on an SDK-staged box.
  - `vike_config::ArmingBlock::NoAccountSupport` is KEPT although no roster venue now produces it: a
    venue scaffolded by `just new-venue` is deliberately not added to `arm_addresses_accounts`, so it
    is the first refusal every future venue's second account meets.
    `crates/vike-mount/tests/unaddressable_account_warning.rs` drives the message from a PLANTED row
    (through the new `vike_mount::unaddressable_accounts_text`) instead of testing a thing nothing
    can produce, and pins the retirement over the whole roster.
  - `crates/vike-mount/src/book_identity.rs`'s dukascopy row keeps `Undeterminable` and its REASON is
    rewritten — the correction §9 of the spec records as owed. The old text rested on an arming
    refusal that no longer exists and on "unknowable", which was never true: the sidecar's ready
    handshake carries an account.
- **Settings-database SCHEMA 2 — the account is a ROW.**
  `docs/superpowers/specs/2026-09-14-the-credential-schema.md` (ACCEPTED, signed 2026-09-14) §4's
  `account` and reshaped `credential` tables and §6's `venue_setting`, plus the in-place migration
  that carries a schema-1 store into them. The account used to live in the key NAME
  (`DUKASCOPY_DEMO1_LOGIN` bakes an account INDEX into the tier token), so it could go stale with
  nothing in the store able to contradict it; it now has a permanent opaque `id` and a column for
  the venue's own answer. **Dukascopy's two demo accounts are two rows** — ruling 1, and the one
  `(venue, tier)` pair in the store that yields two accounts.
  - ⚠ **A schema-2 binary READS a schema-1 store** (`vike_secrets::READABLE_SCHEMA_VERSIONS`), and
    that is what makes the version bump deployable on its own. A bare bump would have been a silent
    all-venues-to-paper event on both live boxes: the reader errors, the infallible wrapper every
    composition root reaches through returns an EMPTY map, and an empty credential map is not an
    error downstream but the LIVE GATE. So the bump and the reshape are not one event — the upgrade
    is performed by `vike-cli secrets migrate`, with `--dry-run` in front of it, and deploy order is
    free.
  - **The upgrade is ONE transaction**, DDL and version stamp included, because `PRAGMA
    user_version` and DDL are both rolled back with the transaction that set them — MEASURED and
    PINNED (`db_tests::the_schema_stamp_and_the_ddl_are_both_transactional`) rather than assumed.
    It has to be: schema 2 keeps `name` and `value`, so `SELECT name, value FROM credential` is
    still valid SQL against it, and a half-applied reshape stamped 1 over schema-2 tables would be
    ACCEPTED by an older binary and answered from. A run that cannot carry EVERY row it read fails
    the whole run and leaves the working schema-1 store — a per-key skip would have committed a
    store missing a credential that exists nowhere else.
  - ⚠ **No reader notices.** `resolve_project` returns the same name→value map before and after,
    proved by comparing the two maps rather than by asserting it
    (`a_reshaped_store_answers_byte_for_byte_like_the_flat_one_it_came_from`). That is only true
    because §11's steps 3 and 4 — the fold of the ten book keys into `account.venue_account_id` and
    the move of the ten config keys to `venue_setting` — are deliberately NOT performed: §12 forbids
    them until a map renderer exists, and the third ordering (move the rows, then fix the readers)
    is the one it calls inadmissible. Every such row is classified and REPORTED BY NAME, so the next
    change takes its work-list out of a run rather than out of prose. `venue_setting` is created
    EMPTY for the same reason — a table is a shape, and a ROW nothing reads is the defect
    `vike_config::CONSUMPTION` exists to refuse.
  - **The account classification is INJECTED**, exactly as `is_node_key` already is:
    `vike-secrets` declares no `vike-*` dependency and stays a layer-15 leaf, so
    `vike_bridge_core::credentials::classify_credential_name` supplies it. §5.2's `POLY_*` split,
    §11 step 2's hand-mapped venue families, §6's non-secret set and §7's book keys are tables
    there, each row citing the section that measured it.
  - **§4.2's superseded values are rescued**: a `#KEY=VALUE` comment line in the credential file
    becomes a `superseded_at` row, and prose comments become `notes`. Both come from a
    comment-ONLY scan that produces no live value, so it cannot disagree with the one parser about
    anything that reaches the map. A commented key with no live row is REPORTED, not written.
  - **The write path learned the shape.** `credential.name` is a PARTIAL unique index now, so
    `ON CONFLICT(name)` no longer names a constraint; the upsert is an explicit UPDATE of the live
    row plus a classified INSERT for a name the store has never held. A NEW name with no classifier
    is refused BY NAME rather than filed as a deployment-level credential belonging to no venue,
    and a NEW name against a schema-1 store is refused with the verb that fixes it. Replacing a
    KNOWN key needs no classifier at all — which is what keeps the venue's own cTrader grant
    rotation working, on both schemas.
  - ⚠ **`{VENUE}_LIVE_*` and `{VENUE}_MAINNET_*` are ONE credential under two names, and the store
    now says so instead of refusing to exist.** `vike_model::account_keys` normalizes the legacy
    `MAINNET` tier onto `LIVE` and §4.4 removes the store's own tier token from `field`, so both
    spellings resolve to one `(account_id, field)` — which `credential_one_live_value` admits once.
    MEASURED end to end on a planted schema-1 store holding both: the dry run said it would be
    upgraded, and the apply failed with `UNIQUE constraint failed: credential.account_id,
    credential.field`, out of `write_rows` as a raw engine error naming no key, i.e. BEFORE the
    count guard that would have named one. On the create path the half-built database is unlinked
    and the verb then fails permanently, with hand-editing `secrets.env` as the only repair — the
    file this whole design promises never to touch. It is reachable rather than theoretical: both
    spellings are legal names `secrets set` will write, `load_credentials_from` reads LIVE and falls
    back to MAINNET, and `save_credentials` never deletes a line, so a box that renamed its keys
    holds both. **The schema is unchanged.** Identical values file the LEGACY spelling as the
    canonical one's rollback copy (`superseded_at`, which is what §4.2 put that column there for),
    so both `name` rows survive and `resolve_project` still answers for either — the reader admits a
    superseded row whose name no live row carries, which the §4.2 rollback copies can never be.
    DIFFERING values are refused with BOTH names in the message, never a value, and the repair is
    an operator deleting one of the two lines. ⚠ That refusal is per-KEY on the FILE path — every
    other key in the edit lands — and whole-RUN on the reshape, which is the disposition
    `reshape_into` already took for every refusal and is not a second rule: its source is the table
    about to be DROPPED, so a skipped row is a credential destroyed.
    `vike_secrets::RowReport::aliases` reports what was filed, by name and never by value.
  - `PRAGMA foreign_keys` is now set AND verified on every connection: it is off by default and
    per-connection, so schema 2's two `REFERENCES` clauses would otherwise enforce nothing.
  - ⚠ **Two deviations from §4's printed DDL, both argued in `crates/vike-secrets/src/schema.rs`:**
    `account.label` is NULLABLE (the signature rules the provisional `DEMO1`/`DEMO2` labels are not
    written at all and that labels are "informative and optional" — and a `NOT NULL` label makes
    dukascopy's sixteenth row un-insertable, which §4.1 says in its own words), and `tier` carries a
    `CHECK` because `STRICT` constrains types and not values.
- **`vike-cli secrets migrate` — the verb that CREATES the settings database and moves the
  credential store into it.** `docs/decisions/0054`'s credential half shipped a schema and a reader
  and nothing that could bring the store into existence, so no box could reach either: nothing
  outside tests called `vike_secrets::migrate`. It reads `secrets.env` and `node.env` and writes
  NEITHER — retiring them stays an operator act — and re-running it inserts nothing.
  ⚠ **The first successful run is irreversible in practice**: from then on
  `vike_secrets::backend_at` answers `Database` for every process on the box, the credential file
  stops being read, and the node-key classification is baked into two tables there is no repair
  verb for. So `--dry-run` is a REAL dry run rather than a courtesy —
  `crates/vike-secrets/src/db.rs`'s `plan` is the ONE classifier and `migrate` is that plan plus the
  write, so a rehearsal cannot describe a migration different from the one that follows it.
  ⚠ **`preview` is that plan PLUS the row classifier**, and it was the plan alone until a reviewer
  measured what that left out: `plan` decides which table a name belongs to and what is already
  stored, and every decision `vike_secrets::schema`'s fill makes PER ROW — which account, and what
  to do when two names resolve to one — was invisible to it. On a store holding both
  `ASTER_LIVE_API_KEY` and `ASTER_MAINNET_API_KEY` the preview printed *would be UPGRADED* and
  *"every key name would still answer exactly as it does today"*, and the apply that followed it
  failed. It now runs the REAL classifier over an in-memory REPLICA of the store (`preview_rows`),
  which creates no file, opens nothing for writing, and — `temp_store = MEMORY` — cannot spill a
  plaintext credential to a temp file, so the two refused previews below stay refused.
  It returns `MigrationPlan` and deliberately not `Migration`, whose
  `database_exists` is a claim about a run that SUCCEEDED. The three cheaper previews that were
  refused — open-for-write-and-roll-back, migrate-a-copy-in-a-temp-dir, just-run-it-it-is-idempotent
  — are argued at `preview`, along with the four things a dry run still cannot promise.
  The migration is journalled as ONE `credential_write` for the ACT (`venue` = `multi`, the untiered
  tier, key NAMES only), which `crates/vike-cli/src/cmd/secrets.rs`'s `record_migration` argues
  against one record per venue and tier. `crates/vike-ops/tests/credential_writer_gate.rs` watches
  the migration by name — the blindness 0054's *What must land* predicted, since a gate keyed on the
  two upsert names goes green rather than red at a store that grows a CREATOR.
  `docs/decisions/0036` carries a fourth amendment narrowing *an ABSENT store is refused, not
  created* to what it always was: a claim about `secrets set`.
- **A remote parameter search names its own method, and a daemon too old to hear that says so.**
  `Request::RunSweepProfile` carries a `search` selector (`vike_datahub_client::proto`'s
  `WireSearch`: the method plus that method's own knob, as the TOKENS the operator typed), so
  `--optimizer euler|tpe|genetic`, `--euler-depth`, `--trials`, `--seed` and `--rank-by multi` work
  on `--addr` as well as `--local` — and `genetic`, which was refused during arg PARSING on both
  routes, joins the roster. Stage 7 of
  `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`. ⚠ The field alone would have
  been the defect rather than the fix: `Request` has no `deny_unknown_fields`, so a daemon
  predating it DECODES the frame, drops the selector, runs the exhaustive grid and answers a
  well-formed report — a Bayesian search and an exhaustive one, indistinguishable from the answer.
  So the client checks `FEATURE_SEARCH_METHOD` in the handshake and refuses BY NAME with nothing
  sent; `PROTO_VERSION` is deliberately NOT bumped, because a bump is strict equality inside
  `connect` and would report "a service could not be reached" for a service that answered, while
  breaking the signed auth mac against every keyed peer not upgraded in lockstep. The ownership
  rule, the value parsers and their refusal texts moved ONCE into
  `vike_backtest::harness::search_select`, which the engine's argv parser and the compute server
  both call — so `--trials` under the wrong method is refused with the same sentence whichever
  route ran, and the three optimizer rosters §5.4 recorded (engine four, `--local` three, wire one)
  are now one const with five test legs, each mutated to prove it bites. MCP's `run_sweep` grew the
  same four arguments: before this, every agent was a grid-search user. A searched sweep also
  reports its own cost — euler's budget line, tpe's trial line — which a remote caller could read
  nowhere at all, because the engine prints it to a stderr no socket carries.
- **`vike-cli research study` runs a study instead of explaining why it cannot.** Ruling R1 of the
  same design makes `research` a fourth PLANE with `study` as its first sub-verb, and a plane whose
  only verb refuses on every box is not a plane — so the wire request that verb was built against
  finally exists: `vike_datahub_client::proto`'s `Request::RunStudy` (the study name, the recipe's
  TEXT — never a path, because the backend is a different machine — and the window as typed) and
  `Response::StudyReport`, the minted run as JSON. `--json` is accepted now that there is an answer
  to shape, which is what its own refusal promised. ⚠ The capability string `FEATURE_STUDY` did not
  change and was never version-gated; what changed is that a daemon can now advertise it — **and
  only when a study runner is MOUNTED.** That runner is INJECTED, because
  `vike_studio_core::study_dispatch`'s `run_study_plan` sits ABOVE the compute daemon in the layer
  graph and the edge cannot be reversed; `crates/vike/src/main.rs` is the one composition root that
  can see both sides, exactly as it already is for the Studio runners. So `vike-backend backtest
  --addr` serves a study, a bare `cargo run -p vike-backtest --bin backtest` refuses it BY NAME,
  and both refusal paths still print the `vike-backend study` invocation with this run's own
  arguments filled in. The store root and the trainer path stay the daemon's configuration and are
  NOT wire fields — a client may not steer a filesystem it cannot see, which is what `--store` and
  `--lightgbm` have been refused by name for since this verb shipped.

- **`vike-cli config set <key> <value>` — the change journal stops being empty.** A live box's
  journal held, across two months, twenty-nine boot records, twenty-eight venue mounts and ZERO
  settings writes, while the credential store's own backup files proved it had changed at least
  eight times. The ledger was never broken: the only journalling settings writers an operator could
  reach were two `backend` sub-verbs, each writing one fixed key, so every real change was made with
  an editor over ssh. The verb writes through `vike_config::set_setting` (comment-preserving,
  loader-validated before a byte lands, atomic) and records through `vike_model::change_journal` —
  the path `vike-cli backend connect` already proves works. `set_setting_journalled` moved out of
  `crates/vike-cli/src/cmd/node/mod.rs` into `crates/vike-cli/src/cmd/settings_write.rs`, so the
  decisions a journalled settings write makes have one home rather than two. A credential-shaped key
  is REFUSED rather than redacted (`vike_config::is_secret_key`), which is what keeps a value that
  would need hiding from `ps` off the command line; there is no `--file`; and a policy RISK CEILING
  demands the retyped key while the ARMING ceilings do not — a line no site in this tree could draw
  before, because the daemon's `apply_set_setting` and the GUI's `can_save_fields` both key on the
  FILE and the two classes share one. `vike_config::requires_typed_confirm` is that line, drawn on
  the KEY and matched by segment rather than by prefix. Every local write is restart-to-apply and
  the verb says so, because nothing in this tree watches a settings file; it also prints the key's
  READ verdict from the same table `vike-cli config show` renders, and names the ABSOLUTE file it
  wrote plus which rung resolved that directory — the project walk has captured the wrong tree three
  times on record and the live box has two project roots.

### Fixed
- **The Dukascopy second account could not be mounted by ANY policy — the feature above shipped
  unreachable — and four defects behind it.** The fan-out mounts the DEFAULT account first
  (`vike_mount::known_accounts` puts it first, `accounts_to_mount` preserves row order), the
  one-sidecar claim was FIRST-COME, and `vike_config::VenuePolicy::account` is `min(venue line,
  account line)` — so raising `venues.dukascopy` to `demo`, the only way to get an
  `[accounts.dukascopy]` line above paper, NECESSARILY arms the default account too. It therefore
  took the sidecar every time and every labelled account hit the claim refusal. On the owner's own
  box, holding both `DUKASCOPY_DEMO1_*` and `DUKASCOPY_DEMO2_*`, there was no configuration that
  traded the second account.
  - **The holder is decided by the POLICY, before anything is mounted** —
    `crates/vike-mount/src/dukascopy.rs`'s `sidecar_holder`. An `[accounts.dukascopy]` line above
    `paper` whose account would arm on its own merits takes the session and **the DEFAULT account
    declines**; a box that names no account keeps mounting `DUKASCOPY_DEMO1_*` byte-identically; an
    account that is named but cannot arm (no such row, no credentials, an unreadable store) takes
    nothing from the account that works. So the whole of mounting Demo2 is now two lines of
    `policy.toml` beside `vike-cli secrets set-book`. ⚠ The cost is stated rather than discovered:
    on a box that arms a labelled dukascopy account, a strategy mounted on the venue's DEFAULT
    account runs on a PAPER engine — loudly (an `error!` naming the holder, and a
    `vike_config::ArmingBlock::SidecarHeldElsewhere` row), but `refuse_unarmed_mount_accounts`
    deliberately does not refuse a mount that names no account.
  - **The arming PROJECTION and the mount agreed about the store and not about the sidecar.** Both
    resolved the account; only the mount consulted the claim, so a two-account box projected `Demo`
    for BOTH accounts, `vike_run::refuse_unarmed_mount_accounts` saw a non-Paper row and passed, and
    the mount then built a paper engine — **a strategy believing it was armed running on paper**,
    the exact failure that refusal exists to prevent. The holder rule is one function both sides
    call, so the disagreement is now unrepresentable.
  - **A FAILED spawn burned the sidecar claim.** It was an `AtomicBool` nothing ever released, and
    `DukascopyExecutionClient::spawn` fails on four ordinary paths (absent jar, `Command::spawn`, a
    `Fatal` envelope from a bad login, the 300s ready timeout) — `crates/bridges/dukascopy/CLAUDE.md`
    records a JNLP 404 firing on roughly every second mount on a box without the retry jar, so this
    was the common path. Every later account was then refused with *this process already runs a
    JForex sidecar* when it ran none. The claim is an RAII guard now: released on drop unless the
    spawn committed it, and exercised over a caller-supplied flag so the release is actually tested.
  - **A store-read FAILURE was swallowed and re-reported as a bad row.** The account KEY-NAME read
    went through `.ok().flatten()`, so a database that existed and would not open produced *this
    row's key names do not say which broker it is* about a row nobody could read the key names of —
    sending an operator to hunt a row that was correct. Both halves of the read now carry their
    error, and `DukascopyRefusal::StoreUnreadable` names WHICH read failed.
  - **`CREDENTIAL_STORE_PIN` had gone BLIND.** The dukascopy resolution opened the credential store
    from a `vike-mount` LIBRARY file, at a location taken from a process global
    (`vike_bridge_core::halt::declared_project_state_dir`) — precisely the class that ratchet exists
    to hold down — and every test stayed green, because neither account reader was one of
    `CREDENTIAL_STORE_READERS`' keyed names. Both halves are fixed and in that order: the family is
    KEYED now (four `vike-bridge-core` wrappers plus `vike_secrets::resolve_accounts` /
    `resolve_account_keys`), and the library read is GONE — the composition root performs it beside
    its credential load and hands the snapshot down as `vike_mount::MountPolicy::accounts`
    (`vike_mount::AccountDirectory`). The pin did not grow: the one root that reads it,
    `crates/vike-tradehub/src/tradehub_cli.rs`, was already pinned. It also makes the whole
    resolution drivable from a test — `crates/vike-mount/tests/dukascopy_sidecar_holder.rs` is a
    two-account box written down as a value, with no database on disk and no process state.
  - **A test that could no longer fail was retired rather than left standing.**
    `crates/vike-mount/tests/unaddressable_account_silence.rs` asserted that a single-account box
    emits no unaddressable-account warning — over a projection that cannot produce one for ANY
    roster venue since dukascopy joined `arm_addresses_accounts`, so it passed for a reason that had
    stopped existing while its own doc claimed it was doing more work than before. It now proves the
    refusal an operator actually meets (the one-sidecar decline), in both directions, over the real
    mount; the retirement it depended on stays pinned by name in its twin.
  - Stale docs corrected: `load_workspace_accounts_from_env` and `vike-secrets`' crate doc both
    still said nothing mounts, arms or signs from the `account` table and that nothing could join
    `arm_addresses_accounts` — false since #1845, and the worst kind of stale doc on a reader that
    decides which LEGAL ENTITY an order reaches.
- **Six unread settings told operators to export an environment variable that is equally dead, and
  `vike-cli config show` printed the sentence.** `vike_config::CONSUMPTION`'s `Consumer::Not` rows
  admit that nothing reads a key and name "the reader that owns the variable today" instead. For
  `flags.{hl_outcome, pm_resolve, poly_auto_redeem, poly_heartbeat, poly_redeem_halt,
  record_chains}` that reader sits inside a poller (or a recorder constructor) **no composition
  root ever builds**, so setting either spelling changes nothing — while the paragraph read as
  *use the variable instead*, printed to an operator who had configured the key. That is
  `Policy::max_total_exposure`'s defect one level down: positive confirmation of something false,
  from the command added to stop it. The gate could not see it —
  `crates/vike-config/tests/settings_are_consumed.rs` checked a `why` for LENGTH and for the
  absence of "todo", and never opened the file it named — and the tree already knew about one of
  them in another gated table (`crates/vike-ops/tests/kill_switch_gate.rs`: "nothing outside tests
  constructs this poller").
  The claim is now DATA: `vike_config::Reader` says whether the environment spelling `Live`s (with
  a caller OUTSIDE the defining file as proof), is `Uncalled` (naming every entry point that must
  have no call site), or is `Nothing`; direction 5 of that gate checks it, so wiring one of these
  features turns its own row red instead of leaving it lying. `config show` and `config set` lead
  with `Reader::verdict` — one derived line, one home, both surfaces — before the paragraph.
  Nothing was deleted: both tombstone shapes would print a SECOND falsehood
  (`REMOVED_FLAG_KEYS` says "export `<VAR>=1` instead"; a `REMOVED_ENV` refusal would stop a daemon
  over a spelling that changes nothing while the adapter's live `env::var` call site stands), and
  each feature needs a MOUNT decision, not a threading change. Four doc comments that asserted the
  deleted wiring outright — `crates/vike-data/src/chain_rec.rs`'s and
  `crates/bridges/deribit/src/dvol.rs`'s "PRODUCTION WIRING", `crates/bridges/deribit/src/chain.rs`'s
  "PRODUCTION PATH (live, not aspirational)", and `crates/vike-app-core/src/tools.rs`'s
  `options_provider` — are corrected; they are how the claims survived the desktop cut. So is
  `tools.rs`'s `spawn_tool_fetchers`, the function `options_provider`'s corrected doc NAMES as its
  caller and which went on asserting the same wiring one screen away.
  `preferences.sweep_threads` stays the table's one `Reader::Live` — its variable genuinely still
  caps concurrency — with its stated obstacle corrected: it had named `vike-datahub`'s backtest
  serve arm as one of three consuming processes, and ruling 7 of the datahub market-data-wire
  design deleted that crate's `vike-backtest` and `vike-studio-core` edges outright, so that daemon
  cannot reach the code at all. The real block is a process-wide resource knob read at rayon pool
  construction from three front doors in two crates, not a missing settings load.
- **…and the gate that was supposed to make all of that un-shippable could not fail where it
  mattered.** Every scan in `crates/vike-config/tests/settings_are_consumed.rs` stopped at the first
  line whose trimmed text began `#[cfg(test)]` — an attribute on an ITEM, not a marker for a test
  module, so a `#[cfg(test)] use`, `fn` or `const` ended the scan having cut nothing.
  `crates/vike-tradehub/src/tradehub_cli.rs` carries one at line 304 of 7,684, so the daemon's whole
  live mount — every `FoldTier` row, `make_engine`, `spawn_recon` — was invisible to the no-caller
  search that makes a `Reader::Uncalled` row mean anything, and "wiring one of these features turns
  its own row red" was false in the one file a mount would be wired in. A/B-measured against the
  real gate binary: the same planted `AutoRedeemPoller::spawn(` call read GREEN at line 2000 and RED
  at line 100. Tree-wide, 823 `crates/**/src/*.rs` files carry a `#[cfg(test)]` and 231,897 lines
  sat after the first one in their files. The fixture-only kill proof beside it passed throughout,
  because the disabling text appears in no three-line synthetic string. `production_lines` is now
  the one notion of production code every direction shares (the cut is at a test MODULE, and an
  inline one is skipped body-and-all with the scan RESUMING after it), which also closes the mirror
  hole — `Reader::Live` accepted a `caller` that existed only inside its file's own test module —
  and `Reader::Uncalled` additionally searches the READ's own name, a door an entry list cannot name
  because `Type::method` qualification structurally excludes a `pub fn`. All three are
  mutation-proved against real files.
- **`vike-cli config show` contradicted itself inside one invocation**: its file half printed
  "NEITHER SPELLING DOES ANYTHING" for six flags while its environment half listed the same
  variables under a header promising "READS = what the reader consults". The env table now names
  the dead rows beneath itself and `--json` carries `reader_verdict`, both derived from
  `vike_config::env_verdict` (FLAG_REGISTRY × CONSUMPTION) rather than a second list.
- **`flags.poly_exec` joined the typed-confirm ceremony.** Its exclusion from `CONFIRMED_FLAG_KEYS`
  rested on "a write of it arms nothing", which stopped being true when the same sweep wired the
  key: a write of it now arms LIVE Polymarket execution, on a venue with no testnet at all.
- **A planted binary survives `ETXTBSY`, everywhere `vike-cli`'s tests plant one — and `[0.1.10]`'s
  claim that CI could not see this is CORRECTED below.** A test that writes an executable and
  immediately causes it to be exec'd fails with `Text file busy (os error 26)`: `fs::write` closes
  its own descriptor, but a binary's cases run as parallel THREADS of one process and a
  `fork`/`posix_spawn` in ANY other thread hands the forked child an inherited duplicate of that
  still-open write descriptor until it reaches `execve`. Measured twice on 2026-09-14, on two
  unrelated PRs that could not touch the file — `data_cli.rs`'s
  `json_is_the_whole_of_stdout_and_the_engines_report_moves_to_stderr` (errno 26 on stderr) and
  `backtest_cli.rs`'s `the_project_bin_engine_is_the_one_that_runs`, which surfaced as an EMPTY
  value under "the child's stdout is INHERITED, not captured:" because the child died before
  producing anything. ⚠ **That second shape is why curing the losers is curing nothing**: the
  planted engine is exec'd by `vike-cli`, one process away, so the errno never reaches this process
  as a number and eleven cases (three in `backtest_cli.rs`, eight in `data_cli.rs` — not the two
  that had lost) were all holding the same coin. ONE shared helper now owns both the plant and the
  spawn (`crates/vike-cli/tests/common/mod.rs`), matching `raw_os_error() == Some(26)` for the
  spawns this process makes and the `(os error 26)` `std` renders for the ones a child reports —
  the same errno, read back out of text because that is the only way it crosses a process boundary.
  Every other outcome is handed back on the FIRST attempt, a missing binary included: a retry that
  masked a real spawn failure would be worse than the flake, and `data_cli.rs`'s
  `a_missing_engine_is_the_connect_rung` exists to prove that failure reaches the operator.
  `crates/vike-cli/tests/planted_binary_retry.rs` measures both directions — a missing binary fails
  in under a quarter of the retry budget, a PERSISTENT busy panics rather than being returned as an
  answer — and `a_case_that_plants_an_engine_may_not_spawn_the_cli_itself` makes "a new case joins
  the cure by construction" a check rather than a hope, because `backtest_cli.rs`'s dominant idiom
  is a direct spawn that would opt straight back out.
- **The settings-write lock budget belongs to the CALLER, so a GUI frame can no longer freeze on it.**
  The advisory lock below shipped its spin budget as a private constant every caller inherited, and
  the argument for that ~3 s was written for exactly one of the three: a CLI at a prompt with no
  supervisor above it. `crates/vike-app-core/src/tool_views/venues.rs`'s `apply_arming` runs INSIDE
  an egui frame, where three seconds is a frozen window with no repaint, no spinner and no cancel;
  `crates/vike-tradehub/src/server.rs`'s `apply_set_setting` runs on a connection thread, where the
  same wait holds a socket and the rate token the peer paid for. `vike_config::LockBudget` is now a
  parameter of `set_setting_within` and each caller argues its own at its own site: the CLI keeps
  the whole-process budget, the GUI takes ONE non-blocking attempt (and the acquire no longer sleeps
  after its final try, so that means zero rather than 2 ms) folding a `Busy` into the refusal row it
  already renders, and the daemon takes 1 s — bounded at both ends by a compile-time assertion
  against the client's own `CONTROL_REPLY_TIMEOUT`, because blocking past a peer's deadline would
  land a write nobody is left to hear about. A caller that says nothing still gets the bounded
  default — and no production caller does, which that spelling's own doc now says, so a fourth one
  arriving through it is a deliberate act rather than an inherited shape. `LockBudget::from_millis`
  also SATURATES: it takes a `u64` and counts attempts in a `u32`, and the cast that used to sit
  there answered a SHORTER wait than asked for above ~8.6e9 ms — silently, on a public API, in the
  one direction a caller cannot detect.
- **A stranded settings write is no longer journalled as `refused`.** `Outcome::Refused`'s own
  definition is "the change was refused and nothing was written", and `SettingsWriteError::Stranded`
  means the previous file was moved to `<file>.toml.bak` and the target is ABSENT — a larger change
  than the write would have been. Both journalling callers mapped it there, reproducing inside this
  branch's own error path the confidently-wrong accountability record the lock was taken to remove.
  `vike_model::change_journal::Outcome::Stranded` is the fourth outcome and
  `vike_config::journal_outcome` the one mapping. The same state's cheap second consequence is
  closed too: an absent target with a `.bak` beside it is REFUSED rather than treated as a fresh
  project and written over with a one-key file. ⚠ Two honesty notes ride with it, both declared on
  the types rather than fixed by changing a row shape.
  `vike_model::change_journal::SettingTarget` now states that on a row whose outcome is not an
  applied one the `old`/`new` cells describe the ATTEMPT and not the file: a refusing writer
  established no previous value, so an absent `old` there means "none was established", never "the
  key was unset" — which on a `Stranded` row would be a false claim beside a now-true outcome.
  And the daemon's wire path appends NO row for a refusal at all: `accept_command` returns through
  a `?` before the audit call, which is this under-recording direction stated where it happens,
  kept because that surface journals no refusal for ANY control verb and recording one for
  `SetSetting` alone would make its own trail inconsistent.
- **The live-arm, control-opening and safety-override FLAGS demand the retyped key.** `config set`
  drew its ceremony on `policy.*`, so `policy.max_leverage` — a field nothing in the tree reads —
  required a retype while `flags.tradehub_live`, whose own doc calls it "the single largest blast
  radius on this list", and `flags.tradehub_control`, which OPENS the headless daemon's
  authenticated control scope — a remote order-origination path — were one-line non-interactive
  writes. All live in `flags.toml`, so no file-keyed rule in this tree could have drawn that line.
  `vike_config::typed_confirm_reason` carries the class, and the refusal names which class it is
  refusing rather than calling a flag a policy ceiling. ⚠ The class's membership rule is the REASON
  that function renders — real money on a real wire, or a default-on guard turned off — and not a
  list sourced from `crates/vike-config/src/flags.rs`'s declared safety overrides: the narrower
  sourcing shipped first, and it left `flags.tradehub_control` OUTSIDE a ceremony that
  `flags.tradehub_allow_public_bind`, which only widens that same surface's bind ADDRESS, was
  inside — the inverse of the ordering the class claims. ⚠ Relatedly, the ruling that lets the
  generated skill pages advertise this verb rested on "every key it writes is restart-to-apply so
  nothing it does takes effect in a running process", which is FALSE on a deployed box:
  `deploy/vike-tradehub-project.service` carries `Restart=on-failure`, the tagged deploy restarts
  the roster unattended, and the upgrade window restarts daily — restart-to-apply is a delay, not a
  gate. The ruling now stands on the description-vs-callable argument alone and says outright that
  the containment is the shell grant.
- **A settings refusal says which of write / refuse / partly-happened occurred.** A failure to open
  the writer's never-unlinked sentinel was blamed on the DIRECTORY alone, which is a dead end when
  the directory is innocent — `SettingsWriteError::Lock` now names the sentinel too, and leads with
  the cause the shipped units GUARANTEE and an `ls` answers wrongly: a settings directory that is
  read-only IN THE WRITER'S OWN MOUNT NAMESPACE (`ProtectSystem=strict` plus a `ReadWritePaths=`
  grant naming only `settings/state`), with the foreign-owned sentinel kept as the second
  possibility rather than the asserted one. MEASURED 2026-09-13 inside the live daemon's namespace:
  the sentinel `touch` is EROFS, `settings/state` is writable. The consequence is declared where it
  bites — every `WireCommand::SetSetting` on a deployed box refuses there before any of
  `vike_tradehub::server::SETTINGS_LOCK_BUDGET` is spent, so that budget governs a container or dev
  configuration rather than the the CI box deployment, and whether the daemon should be able to write its
  own settings at all is an owner ruling in flight rather than a grant to widen. The loader
  `Validation` refusal renders a message naming a real file
  and nothing else, which reads as "I have just broken policy.toml" when not a byte moved, so it and
  the unknown-section refusal now lead with "nothing was written". And `config set`'s `wrote …` line
  is absolutised, because the one rung that can be relative is the one a human types.
- **A settings write is now serialised against the GUI and the daemon.**
  `vike_config::set_setting` was a read-modify-write with NO cross-process serialisation while the
  ledger that records it IS serialised (`vike_model::change_journal`'s `append_record` takes an
  `AppendLock`), so two of its three production callers racing on one file produced TWO journal rows
  both stamped `applied_pending_restart` and ONE surviving value — an accountability record that is
  confidently wrong, which is worse than recording neither. The whole read → edit → validate → land
  sequence now runs under an exclusive advisory lock on `vike_config::SETTINGS_LOCK_FILE`, the
  `AppendLock` idiom (`std::fs::File::try_lock`, kernel-released when the holder dies). It SPINS
  with a bounded budget and then REFUSES rather than blocking: the change journal's blocking acquire
  rests on a microsecond critical section with no caller code inside it, while this one is a read, a
  parse, a loader validation and two renames held against processes this one cannot see, and a CLI
  that hangs during an incident has no supervisor above it. ⚠ It closes the LOST-UPDATE window, not
  the two-operation one: the file write and the ledger append are still separate, so a crash between
  them leaves a changed file and no row — the under-recording direction, unchanged from before.
- **A failed atomic landing no longer DELETES the settings file.** `set_setting`'s rename fallback —
  there for a destination another process holds open on Windows — did `remove_file(&path)` and
  retried, so a second failure left `policy.toml` gone with nothing in its place: the box boots on
  compiled-in defaults, with no file to inspect and no backup. It now moves the previous file aside,
  retries, and puts it back; the one case where the restore itself fails is its own error variant
  naming where the operator's bytes are.
- **`vike-cli config set`'s missing-VALUE refusal no longer echoes the operator's token.**
  `KEY=VALUE` is the dotenv muscle memory that produces a one-positional command line, and the
  credential gate ran after the parser — so `config set BINANCE_LIVE_API_KEY=sk_live_…` printed the
  key in full, twice, on stderr. The token is now tested both whole and pre-`=`, and a match quotes
  nothing. `--` also ends option parsing, so a VALUE may begin with dashes and a value spelled
  `--help` is written instead of printing usage and exiting 0.
- **The credential fence moved from the leaf caller to the shared writer**
  (`crates/vike-cli/src/cmd/settings_write.rs`), so a third caller inherits it instead of
  re-proving it; a mistyped SECTION is diagnosed as a section error rather than as a credential; and
  both refusals now name `vike-cli config show` as well, so an operator who mistyped a SETTINGS key
  is no longer routed into a credential store that refuses it too.
- **A parameter search now leaves a run artifact.** Before this the sweep branch of
  `crates/vike-backtest/src/backtest_cli.rs`'s `run` returned success one statement before the clock
  read that mints a run id, so a grid, euler, TPE or genetic search — the runs that cost hundreds of
  backtests and carry a seed and a budget — wrote nothing at all. A search now mints ONE parent run
  under `<project>/user_data/runs/` holding its identity, a per-trial JSON-Lines ledger written as
  each evaluation completes, a resolved trials document and the common manifest. `<id>#<n>` is a
  LEDGER LINE index rather than a directory, so a 512-point sweep is one Studio listing row and not
  512 — pinned from the listing's own side by
  `crates/vike-studio-core/src/listing.rs`'s `a_search_parent_lists_as_one_row_whatever_it_holds`.
- `backtest trials <id>`: read a finished or interrupted search's trials — `--sort`, `--top`,
  `--json`, `--export-params`. Artifact-only: no store, no profile, no network.
- `backtest --resume <id>`: continue an interrupted search, evaluating only what its ledger is
  missing. It is a memoising `PointEvaluator` rather than a resumed searcher — every method's loop
  is seed-deterministic, so replaying it against the original scores reproduces the original
  trajectory — and no `Optimizer` signature, `SearchOutcome` field or `SweepRow` field changed.
  Refused by name, naming the field that moved, if the profile bytes, the store path, **what the
  store HELD**, the searching binary, the optimizer, the seed or the budget differ from the run
  being resumed. The data witness is the load-bearing one: without it, a search killed part-way, a
  backfill inside the profile's own window and a resume would rank cached scores computed on one
  dataset against fresh ones computed on another, with no error and no field in any document from
  which a reader could detect it. `--resume` with `--keep-trials none` is refused as a
  contradiction, because it would otherwise rewrite the resumed run's header to say it kept no
  ledger and make every later resume of that id refuse for a reason that is not true.
- `backtest --keep-trials none|scalars`. `series` is refused by name, with the reason: a trial's
  equity curve is dropped before a sweep row exists.
- **A box name in a shipped string literal now fails on the PR instead of at TAG time.**
  `crates/vike-ops/tests/shipped_box_name_gate.rs` scans every `crates/**/src` file for the same
  token set `scripts/forbidden_tokens.ere` carries, and it is a TOKENIZER rather than a line
  scanner because the incident that motivated it put the tokens on backslash-continuation lines
  holding no quote character at all — the axis a key-matching line scanner cannot see, and the same
  one the `toml` dev-edge was added to close for build settings. `proc-macro2` lexes standalone and
  discards comments, so only real string `Literal`s are judged; doc comments desugar to
  `#[doc = "…"]` and get their own arm; a `cfg` is EVALUATED rather than matched, so
  `cfg_attr(test, …)`, `cfg(any(test, feature = …))` and `cfg(not(test))` are each classified by
  whether the code SHIPS. No exception table.
  ⚠ **Why no existing gate could see it:** `scripts/publish_mirror.sh` REDACTS those tokens out of
  every text file before it greps, `api-docs` documents that redacted copy, and
  `compile_time_path_gate` polices `env!` embeds only. The one check that would have caught it,
  `scripts/refuse_box_paths.sh`, runs over BUILT RELEASE ASSETS inside `release.yml` — and a
  tag-push run executes the sources as of the tag, so a fix landing afterwards cannot be re-run into
  it and the tag has to be cut again.

### Changed
- **`vike-cli secrets template` is backend-aware.** Its documented use is
  `vike-cli secrets template > settings/secrets.env`, and every doc and skill in this tree names it
  as HOW TO CREATE THE STORE — so on a box whose settings database answers, that redirect wrote a
  file nothing reads and exited 0, the live gate wearing the fresh-install answer. The grid still
  goes to stdout (it is a SHAPE, and reading key names is not a write) and a finding now goes to
  stderr naming the database and the two verbs that act on it. ⚠ On an unmigrated box the output is
  BYTE-IDENTICAL, stdout and stderr both, and a test asserts it: that grid lands in real stores.
- **The profile's parameter-search section is `[paramscan]`; `[sweep]` keeps loading, permanently.**
  Ruling R2 of the backtest-CLI-surface design renamed it on a measurement rather than a
  preference: nineteen competitor CLIs were surveyed and exactly one uses the word "sweep" (an npm
  package), while QuantRocket calls it `paramscan`, LEAN `optimize` and Freqtrade `hyperopt` — the
  last of which would LIE here, because it names Bayesian search specifically and this engine has
  grid, euler, tpe and genetic. ⚠ **The old spelling is a `#[serde(alias)]`, not a deprecation with
  an end date.** `BacktestProfile` is `deny_unknown_fields`, so without it every profile already on
  an operator's disk would fail to load with "unknown field" — the `VIKE_MAX_ORDER_NOTIONAL`-removal
  shape applied to a file. `both_the_new_section_and_the_legacy_one_load_and_parse_identically` is
  the warranty. Every operator-facing string — the usage text, `vike-cli init`'s scaffolded
  profiles, the refusal messages, the MCP tool schema and both generated agent skills — says
  `[paramscan]` now. ⚠ **The Rust identifiers moved too, in the same release**: the client-side
  presence check is `declares_a_paramscan_grid`, `BacktestProfile::is_sweep` is `is_paramscan`, and
  the wire verbs are `Request::RunParamscan`/`RunParamscanProfile` and
  `Response::ParamscanResult`/`ParamscanReport`. What did NOT move is every NEGOTIATED TOKEN — the
  serialized wire tags (pinned with `#[serde(rename = "RunSweep")]` and its three siblings), the
  `"sweep"` field key inside `RunParamscan`, the capability strings `run_sweep_profile`/`run_sweep`,
  the MCP tool name `run_sweep` and `WalkforwardCfg`'s `search = "sweep"` value. A token is
  compared literally by a peer that shipped before the rename, so renaming one refuses that peer;
  an identifier is compared by nobody. The `harness::sweep` MODULE path is unmoved for the
  third reason — a file rename is a separately-scoped edit this tree's path-keyed gates make
  expensive.
- **`vike-cli mcp`'s compute tools read `config.backtest_addr`.** They resolved
  `vike_config::DEFAULT_BACKTEST_ADDR` as a compiled-in default with only their own
  `--backtest-addr` above it, so a box that set the key moved `backtest` and `research study` onto
  it and silently left an agent's `run_backtest`/`run_sweep`/`run_walk_forward`/`list_strategies`
  on `7880`. §18 row 9 of the design recorded it; the ladder is now the same three rungs every
  other compute dialer has.
- **The settings keys that nothing read now have readers, and the environment still beats the file.**
  `config.journal_dir`, `flags.poly_exec`, `flags.poly_reconcile`, `flags.hyperliquid_hip3`,
  `flags.record_properties`, `flags.allow_withdraw_keys`, `flags.preflight_skip`,
  `flags.reconcile_balance` and `flags.reconcile_generate_missing` were declared, validated, and
  reported by `vike-cli config show` as the origin of an effective value while their variables were
  read in libraries no settings file could reach. Each is now resolved once by the daemon and folded
  into the map its reader already consults.
  ⚠ **Precedence is the whole of the design and it is machine-checked**, because
  `docs/decisions/0054` makes "one image, many containers, configured by `Environment=` lines" a
  constraint and the CI box carries its reconcile policy and balance-seed flag in a root-owned systemd
  drop-in. `vike_config::load` applies the environment over the file, and the fold OVERWRITES every
  key whose reader had no credential-store tier — the two Polymarket gates keep theirs, and keep it
  only because a `POLY_EXEC`/`POLY_RECONCILE` line in `secrets.env` REFUSES startup rather than
  outranking anything. `a_process_env_value_beats_a_file_value_for_every_wired_key` is the
  table-driven proof; the two safety overrides (`allow_withdraw_keys`, `preflight_skip`) carry a
  second one each on the reader's own side, where an exported `=0` over a file `true` must resolve
  to REFUSE.

### Removed
- **Seven settings keys nothing read were DELETED, and a deployed box can be refused by one of
  them.** ⚠ **OPERATOR ACTION BEFORE UPGRADING** — a green tag deploys to the CI box automatically, and
  these refusals are hard startup failures:
  - `<project>/settings/flags.toml`: remove `poly_presubmit_register`, `poly_rate_gate`,
    `poly_chain_watch`, `poly_chain_proxy`, `bybit_fast_exec`, `binance_trade_lite_fill`. The daemon
    refuses to load a `flags.toml` carrying any of them, on `true` AND on `false`.
  - the process environment (a unit's `Environment=`/`EnvironmentFile=`, a shell export):
    unset `VIKE_STATE_DIR`. A set, non-empty value refuses startup by name.
  The six flag keys were only ever MIRRORS — the resolved field had no consumer, while the
  variable each mirrored is still read today by the venue adapter that owns it, so the VARIABLES
  keep working and only the file keys are gone (`vike_config::flags::REMOVED_FLAG_KEYS` is the
  tombstone, and the refusal names the variable to export instead). `VIKE_STATE_DIR` is the
  opposite case: its one reader, the desktop's strategy-state sidecar resolver, went with the
  desktop's local core, so the variable itself is a `vike_config::REMOVED_ENV` row.
  ⚠ `VIKE_STATE_ROOT` is a DIFFERENT directory and is not a replacement for it.
  The tracked surface is clean — no `deploy/` unit sets any of the seven, and the repo's own
  `settings/flags.toml` had them commented out — so what has to be checked is the live,
  operator-edited `<project>/settings/flags.toml` and any drop-ins on the deployed boxes.

### Fixed
- **Both committed marketing transcripts had gone stale, and one of them showed a command that no
  longer runs.** `.trader/shots/agent-cli-backtest.txt` typed
  `vike-cli backtest --local …`; `backtest` grew a mandatory sub-verb, so on today's `main` that
  spelling is an EXIT 2 — a published page demonstrating a command a reader cannot run. Its
  `--help` roster was stale in the same edit's wake (it listed `sweep`, `walkforward` and
  `strategy-status`, and none of `report`, `research`, `backend`, `datahub` — eleven verbs where
  the binary prints twelve). ⚠ The sibling `agent-mcp-session.txt` was stale too, and INVISIBLY:
  every command in it still works, but the tool roster it prints had grown from 17 to 23 and the
  handshake now returns the server's `instructions` block, so nothing about running it would have
  said so. **Both were RE-CAPTURED, not edited** — `VIKE_SMOKE_CAPTURE_DIR=.trader/shots
  scripts/cli_mcp_smoke.sh` on a the CI box lane, the one sanctioned route
  (`scripts/marketing_shots.sh`'s `*.txt` arm VERIFIES the committed file and deliberately
  re-derives nothing), transferred by the sha256 that script prints beside each file and checked
  against it. Hand-patching the command line would have left a transcript claiming a session that
  never happened, with output no command above it produced; `.trader/shots/manifest.json` calls
  that "the same lie in monospace". The manifest's two `captured` rows carry the new date and
  commit, and both `claim` counts move with the captures they evidence.
- **The desktop's whole hist-store plane dialled the datahub unauthenticated**, so the Data Manager
  and Studio read an empty store from every remote deployment while the log said exactly why.
  `RemoteHistStore::with_keys` had no production caller at all: both dial sites used
  `RemoteHistStore::new`, whose own doc says to use the keyed constructor against a keyed server.
  ⚠ The severity is structural rather than situational — `vike_datahub_client::bind::bind_decision`
  REFUSES to start a datahub bound to a non-loopback address without keys, so every datahub
  reachable off-box is necessarily authenticated, and the loopback dev case kept working and hid it.
  Same defect CLASS as the DOM-ladder fix one layer over: the market-data half had been wired to
  `connect_authed` and the store half was left behind. One helper (`remote_hist_store`) now serves
  both sites so they cannot answer differently, with the observe-key NAME resolved on the frame
  thread beside the ADDRESS — both follow the ACTIVE backend, and reading them at different instants
  signs one backend's datahub with another's key.
  ⚠ A third dial stays unauthenticated ON PURPOSE and now carries a tombstone saying so:
  `vike_app_core::backfill_wire`'s `run_wire_backfill` sends `Backfill`, a CONTROL-scope verb, and
  control scope also carries the verbs that compile client-supplied Rhai — so this binary must never
  hold that key. It fails closed, naming the missing keys, rather than doing nothing in silence.

## [0.1.24] - 2026-09-12

### Added
- **`vike-backend datahub` serves a live DOM and a live trade tape over the node protocol.** §4 and §8
  of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`: the shared vocabulary
  (`MdSpec`, `MdFrame`, `BookSnapshot`, `WireStreamStatus`, `MdRefusal`, `MdBye`) lives in the LIGHT
  crate, `crates/vike-datahub-client/src/market.rs`, so both ends cannot drift on a spelling; the
  daemon side is `crates/vike-datahub/src/md/hub.rs`'s `MdHub` plus its per-session mailbox. The
  plane is ARMED by the exact string `VIKE_DATAHUB_LIVE=1` and COMPILED by `live-feeds` plus one
  `live-<venue>` feature per bridge — but `MdHub` itself is deliberately feature-FREE, because it
  appears in `serve_authed`'s public signature and cfg-ing the `MdSubscribe` arm away is exactly what
  the decode-vs-drop contract forbids: a build with no plane must DECODE the verbs in order to refuse
  them cleanly, answer `Response::Error` and leave the connection POSITIONAL. That also puts the hub
  suite on the default build, in the roster lane, on every PR. Every constant carries its measurement
  at its declaration (§12 of the design, four one-hour windows of real BTCUSDT depth and tape), and
  the mailbox is bounded in BYTES as well as frames: a frame cap alone makes the box's ceiling a
  CLIENT-chosen parameter — 200 levels a side is 11.2 MB to 42.9 MB and breaks the under-32 MB claim
  `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` leans on — and cannot bound
  a `Trades` frame at any depth, whose size is a function of how far behind the publisher fell.
  `MD_TAPE_BATCH_MAX` was RETIRED rather than given a number, because splitting a drain would break
  the one-frame-per-key-per-tick property the whole derivation rests on. The memory claim is a
  `const _: () = assert!` rather than a paragraph.
- **The desktop's DOM ladder, trade tape and off-daemon charts come back — over that wire, not over a
  local venue mount.** The `fat` deletion took the desktop's local market-data plane and left
  everything downstream of it standing: `BookStore`, `TradeStore`, `GuiFeedSink`, `TickVolAgg`,
  `OrderflowAgg` and the whole of `feed_lifecycle.rs` all compiled, all CI-tested, and fed by nothing
  — `GuiFeedSink`, the struct built precisely to be the core-free market-data seam, had ZERO
  production constructors, and `ensure_depth` took its `no feed registered for venue — the DOM ladder
  will stay STALE` branch every frame for ever. So the desktop was a monitor: positions, orders and
  equity over the node wire, plus bars for the ONE pair the daemon happens to be trading
  (`crates/vike-tradehub/src/publish.rs`'s `project_bar_series` filters on the primary mount). One
  connection to the backend's datahub now feeds all three. The state machine is in
  `crates/vike-app-core/src/md_session.rs` and `datahub_feed.rs`, never in the GUI shell, because
  `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI` names `vike-desktop` — a thread-and-socket state
  machine there is one nobody runs. Three decisions are load-bearing rather than stylistic: the
  desired/served diff compares `MdSpec::key()` and never `MdSpec`, since the client sends
  `depth_levels: None` and the server echoes its AUTHORITATIVE spec with the depth RESOLVED, so a
  whole-struct diff differs on every key for ever and re-dials the live server once per pass; every
  `TradeTick` is re-stamped from the envelope, because the hub blanks `tick.symbol` on the way in and
  `TradeStore::push` keys on that field, so passing wire ticks through puts every print under
  `(venue, "")` and silently empties every tick/volume chart and orderflow overlay; and receipts are
  stamped with the LOCAL clock, never `BookSnapshot::venue_ts`, since two boxes' clocks differ and a
  wire stamp makes every ladder permanently stale or falsely live across a tunnel. The dial is
  AUTHENTICATED for the first time — `BackendRecord::datahub_observe_key`, resolved
  process-environment-first then `<project>/settings/node.env` on the same ladder
  `crates/vike-cli/src/lib.rs`'s `datahub_keyring` walks — and only the OBSERVE half is ever
  populated, structurally: the datahub's CONTROL scope carries the verbs that compile client-supplied
  Rhai, and a GUI that wants a DOM ladder must never hold that key. ⚠ **Not a visual verdict**: no CI
  runner has a GPU, so whether the ladder, the tape and the chart actually PAINT is `VIKE_SHOT` and
  `just qa-shots` against a real datahub, with a human looking.
- **`backtest --optimizer grid|euler|tpe|genetic`, all four on one `Optimizer` seam.**
  `crates/vike-backtest/src/harness/optimize.rs` was READ OUT OF the three existing searchers rather
  than designed ahead of them — they share exactly one evaluation unit, one ordering rule, one
  profile builder and one bounded executor, and none of the four you would expect — so
  `crates/vike-backtest/src/search.rs` does not change by one line, which is the sharpest available
  evidence the seam is real. What it retires is the plumbing each lane re-rolled: three hand-rolled
  copies of the ordering rule each commented as matching the others, and ONE error (`strategy.params`
  not being a table) with THREE different fault timings — the grid failed before any backtest, euler
  latched it and re-raised after the whole depth budget, TPE failed at trial 1 and discarded every
  row it had collected. The shared preflight collapses those into one and is what makes
  `PointEvaluator::evaluate` infallible BY TYPE. `harness/genetic.rs` is the first implementation
  written AGAINST the trait and cost `optimize.rs` no change at all; its generator is COUNTER-BASED
  rather than a stream (every decision is a pure hash of `(seed, purpose, generation, slot)` through
  MurmurHash3's frozen `fmix64`), so how many draws one operator consumes cannot move another's — the
  FORK failure a stateful stream already produced once — and its genome is INDICES, so every operator
  is integer arithmetic and "does the searcher find the grid's argmax" is an EQUALITY rather than a
  smoke test. `crates/vike-ops/tests/clock_pin.rs` PERMITTED that with no exemption row of any kind:
  it refuses a manifest line, not randomness, and a seeded generator whose seed is a parameter is an
  INPUT. Hence `GeneticConfig::new(seed)` takes the seed as a required parameter and `--optimizer
  genetic` with no `--seed` is REFUSED rather than defaulted — a defaulted seed is a hidden constant
  that changes results silently — while tpe keeps its default of `0` because that default is a
  SHIPPED WIRE (`crates/vike-cli/src/cmd/backtest.rs` forwards `--seed` only when the operator wrote
  one). `--seed` is therefore the first flag with two owners, and `METHOD_FLAGS` is keyed on a SET of
  owners so the refusal names every one of them.
- **`vike-cli report` and `vike-cli study` ship as client halves that currently REFUSE on every box,
  and the refusal is the deliverable rather than a shortfall.** Ruling 16 states the rule `vike-cli
  <X>` asks the backend to do X, and measuring the two rosters found two operations with a backend
  verb and no client side — `report` sharpest of all, because a tearsheet is computed from the LIVE
  JOURNAL the trading daemon writes on ITS box, so getting a report on your own live trading meant
  opening an SSH session to production. Neither can be served by what exists, and they need new
  requests on DIFFERENT daemons, which is why the halves are asymmetric: `report`'s wire verb ships
  here (`Request::Tearsheet` / `Response::Tearsheet` / `FEATURE_TEARSHEET` plus `remote_handle`'s
  `tearsheet`), with the node's one arm declining honestly while `served_features` advertises
  nothing; `study`'s request variant is deliberately NOT added, because ruling 7 was moving every
  compute verb off the data server in a sibling change and adding it would have landed it on the
  surface it is about to leave. Each refusal names the missing capability, states that nothing went on
  the wire, and prints the `vike-backend` invocation that does the same work on the box holding the
  data with this run's own flags carried across — a `--seed` dropped in translation rescales every
  return ratio in the answer without saying so. The recipe crosses the wire as TEXT, never as a path,
  because the backend is another machine and a path would resolve against ITS filesystem;
  `--store`/`--lightgbm` are refused BY NAME for the same reason.
- **A recorded series can now be judged on its RATE, not only on its silence.** The binance perp depth
  lane below recorded at 4% of its true rate for forty days and every watchdog read healthy, because
  `crates/vike-recorder/src/alerts.rs`'s `watchdog_tick` measured RECENCY and that lane never went
  30 s without a row against a 300 s default. `Liveness` already carried a monotonic `rows`, so the
  observed rate was always free; the other operand did not exist.
  `crates/vike-data/src/series_cadence.rs`'s `SERIES_CADENCE` now declares one row per recorded
  series naming a CLASS and, where the code declares one, a CEILING — and `Cadence::ceiling_per_s`
  answers `Some` only for a `Sampled` row carrying an interval, which is the whole safety property:
  an unclassified or unmeasured series yields no number, so a consumer has nothing to threshold and
  cannot invent one. It is NOT a field on `StoreKind`, and the evidence is in this tree: inside one
  kind, `trade` spans binance BTCUSDT.P at 20.3–62.7 items/s and one polymarket token at ~0.25/s, and
  inside one (kind, venue) cell `DEPTH_FRESHNESS_THRESHOLD`'s 2026-07-11 sweep found liquid pairs at
  ~0.1 s and the thinnest actively-traded pairs at 44–47 s — 0.0067 updates/s, sixty times slower
  than the lane this work exists to catch. So the judgement is licensed rather than absolute: a
  sampled lane is judged only while its own instrument's trade tape ran at or above the sampling
  ceiling for the whole 900 s window, and then the expectation IS the ceiling with a floor at 20% of
  it. `@depth@100ms` is a periodic SAMPLING of an event stream, not a periodic publish — it declares
  a ceiling, and the floor is the instrument's liquidity, which no static table can hold. What it
  would have done, from measured numbers: the broken lane at 0.42 items/s against a 2.0/s floor
  alerts on the first completed window, about fifteen minutes in; the healthy same lane at 9.8
  applied diffs/s is quiet at 4.9x the floor; the thinnest measured pair is NOT JUDGED at all.
  Delivery is by alert only — the slow set is kept out of `watchdog_tick`'s return value so it can
  never drive `--exit-on-silence`, because degrading a tape is bad and turning it into no tape is
  worse.
- **...and a whole recorded FAMILY going dark is now a page, reached by changing the subject rather
  than inventing a ceiling.** On 2026-08-05 a Polymarket `book` family wrote 117,724 rows in one
  minute, 90,540 in the next and 86,894 in the next — then 782 rows across the following eight
  minutes, five of them completely dark, while four to six members stayed subscribed throughout, and
  nothing said anything. Recency could not reach it: every member had produced rows inside the 300 s
  threshold, and the family ROTATED at least twice inside the dark span, so the tokens born into the
  collapse had nothing to be stale against. Cadence could not, and MUST NOT BE MADE TO — that lane is
  `Cadence::EventDriven`, whose `ceiling_per_s` returns `None` precisely so no consumer can invent a
  number, and `crates/vike-ops/tests/family_collapse_independence_gate.rs` fails the build if the
  family rule so much as NAMES that table. The FAMILY key is the only subject with continuous
  existence across a rotation, so a dying member's tail and its successor's birth land in one total
  and the ~576 legitimate token deaths a day are invisible BY CONSTRUCTION — not by a tuned grace
  window and not by an exemption anybody maintains. The accumulator is a SUM OF PER-KEY DELTAS and
  never a delta of a sum, because differencing a summed counter across a changing key set reads
  zero-or-negative on a HEALTHY rotating family, every window, for ever. Each constant carries its
  measurement (`FAMILY_WINDOW_MS` = 30 s, derived from the shortest measured event — a 300 s tile at
  the wrong phase reads 0.175 and 0.157 of baseline on the motivating incident and MISSES IT
  ENTIRELY; `FAMILY_RING` = 20 windows; `MIN_BASELINE_ITEMS` = 5,000 as a RESOLUTION gate, not a
  venue classifier), and the 1/200 floor is stated as a judgement with its measured void underneath:
  across 23,154 judged healthy windows on 13 lane-days the worst healthy ratio is 0.0932, every
  incident onset window is 0.000000, and the fire count on eight clean days is ZERO at every
  threshold from 0.05 down to 0.001. A verdict is taken only under a LICENCE — some series outside
  the family produced items in the same window, and the witness must have reached its own learn
  ratio, so a host-wide outage cannot license itself on the stream-status markers a dead feed emits.
  It is a fourth rule id (`recorder-family-collapse`) with its own `AlertSignal`/`RuleTrigger` pair
  carrying an item COUNT and no per-second figure at all, because `expected_per_s` MEANS a declared
  cadence and putting a learned baseline there smuggles an invented number into the one place the
  rate rule exists to protect. ⚠ **It is the COMPLEMENT of the rate watchdog, never its substitute**:
  a fault already present when the baseline was learned is invisible to it — this ring would have
  learned the broken depth rate as normal within ten minutes and said nothing for all forty days —
  and its other declared blind spots (any shortfall shallower than ~200x, one member of a healthy
  family, a decay slower than the ring, the first ten minutes after a restart, any family under the
  resolution gate, and correctness as opposed to volume) are stated on the method, on the trigger and
  in `docs/ops/recorder-deploy.md`, because that honesty is the feature.

### Changed
- **⚠ BREAKING (CLI): `state` is gone from the `trade` REPL, `strategy-status` is gone from the top
  level, and the replacements are `status` / `halt` / `resume`.** The REPL's `state` was a READ and a
  WRITE on one word — `state` printed the trading mode, `state halted` HALTED A LIVE DAEMON, and the
  only thing between them was a second token — so a destructive action was reachable by adding a word
  to a read. It is removed outright: no alias, no deprecation shim (this workspace's no-shims rule),
  so an old runbook line now fails as an unknown command rather than doing something else. In its
  place, `halt` and `resume` each say in their own name that they change something, and `status`
  answers the whole "what is this node doing" question in ONE output — the trading mode, the daemon
  identity, its effective params and one row per mounted strategy — which is why it also subsumes the
  retired top-level `vike-cli strategy-status`. The same three words work at the prompt and as
  `vike-cli trade <verb> --node <host:port>`, so a halt can go in a runbook or a unit file. **Pass
  `--yes` in a script**: without it the one-shot reads one line from plain stdin, and a non-terminal
  stdin has three outcomes rather than one — an EOF is a no, a `y` on a pipe CONFIRMS, and a stdin
  that never delivers a line blocks. Typing the removed `state` at either surface refuses and names
  all three replacements, so a stale runbook line does not cost a search mid-incident.
  `Reducing` keeps no CLI verb — a halt admits position-covered reduces, so `market-exit`/`flatten`
  still work — and stays reachable through the `set_trading_state` MCP tool and Telegram's
  `/state reducing`. `trade status --json` nests the node's `WireStrategyStatus` verbatim under a
  `strategy_status` key beside the mode, so the wire half of the payload is unchanged for a script
  that reaches one level in; a half that could not be READ is a `trading_state_error` /
  `strategy_status_error` key and a stderr line, never an omission, and a node too old for the
  `strategy-verbs` capability still gets asked for — and still reports — its trading mode. Ruling 17
  of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`.
- **⚠ BREAKING (DEPLOY): the recorder and the data server are ONE daemon, and `vike-backend recorder`
  is retired.** Ruling 10: both are data-management processes and neither is whole — the recorder OWNS
  venue subscriptions and has no network surface at all, the data server has the wire and no feeds.
  The new spelling is `vike-backend datahub --record <profile>`; the profile format is UNCHANGED, so
  an operator's `settings/recorder.toml` parses exactly as it did and only the word in front of it
  moved (`--profile` → `--record`). ⚠ **The justification is RESPONSIBILITY, not socket economy**, and
  every file carrying the argument says so: measured before the merge, the trading daemon held bybit
  BTCUSDT while the recorder held polymarket plus binance BTCUSDT.P — three venues, overlap ZERO.
  A library edge was unavailable in EITHER direction (both crates declared `layer = 65`, and
  `layer_gate` fails when `to >= from`), so one file moves and the tier follows the ROLE: the
  recorder's daemon body becomes `crates/vike-datahub/src/recorder.rs`, its `[[bin]]` retires, and
  `vike-recorder` drops 65 → 55 as a library the data daemon composes. With no `--record` the process
  is byte-identical to before the merge; with `--record` the RECORDING keeps the main thread and
  `serve_authed` moves to a spawned one, deliberately that way round, because the serve loop has no
  teardown at all while the recorder's teardown is where the buffered tape is either flushed or lost.
  ONE STORE: the profile's `store` key and the root the server resolved must name the same directory
  or the daemon REFUSES to start, since a recording landing in a root the wire does not answer from is
  tape nobody can read. `vike-backend recorder` is now `Route::Usage(2)` — exit 2 with a tool list,
  never a 203/EXEC — so a stale unit fails loudly; `deploy/vike-recorder.service` and its project twin
  are gone, `deploy/vike-datahub.service` gains the stop budget a serve-only daemon deliberately did
  not have, and `deploy/vike-datahub-project.service` is the recorder's project unit renamed with
  `--record` on its `ExecStart`.
- **⚠ BREAKING (CLI/DEPLOY): the seven compute verbs leave the data daemon for `vike-backend backtest
  --addr`.** Ruling 7 splits one served surface into TWO daemons speaking ONE protocol:
  `RunBacktest`, `RunSlice`, `RunSweep`, `RunWalkforward`, `RunSweepProfile`, `RunWalkforwardProfile`
  and `ListStrategies` move; the store verbs stay. Measurably, co-hosting put the DOM and the tape
  inside a sweep's `MemoryMax=8G` kill radius, so a sweep an operator started could take the market
  data with it and then re-dial every venue socket under `RestartSec=5` — two processes now, two
  ceilings, and neither can reach the other's. THE ENGINE DID NOT MOVE and never lived in
  `vike-datahub`: that crate FORWARDED to `vike-backtest`, and what changed is which socket answers —
  with the consequence that the data server drops its `vike-studio-core`, `vike-script` and
  `vike-backtest` edges and no longer compiles a client-supplied strategy at all. The three Studio
  slice verbs are dispatched through an injected table whose type names only the `Wire*` DTOs (the
  `BackfillTable` seam this protocol already uses), mounted by the multicall dispatcher, because
  `vike-studio-core` (55) depends on `vike-backtest` (50) and the edge could never point the other
  way. Everything a second copy would eventually contradict moved DOWN into `vike-datahub-client` —
  `plane_of`, `required_scope`/`scope_admits`/`VerbScope`, `request_kind`, `wrong_plane_message` and
  the whole bind guard in the new `src/bind.rs` — with no `pub use` shim, and a verb sent to the wrong
  daemon is ANSWERED rather than dropped, by one helper that writes both refusals so they cannot
  drift. Daemon mode is `--addr` with an OPTIONAL value — the flag says become a daemon, the value
  resolves `--addr <v>` → `VIKE_BACKTEST_ADDR` → `config.backtest_addr` → `DEFAULT_BACKTEST_ADDR`,
  folded by `vike_config::Config::apply_env` so one loader decides precedence rather than a second
  ladder inside a binary.
- **⚠ BREAKING (CLI): `vike-cli sweep` is deleted, and the five data-management flags leave
  `backtest`.** Rulings 12 and 13. `--fetch`, `--fetch-starter`, `--seed-demo`, `--export` and
  `--rm-series` are none of them about backtesting — they fetch from venues, write the store, export
  from it and DELETE from it — so they become `vike-cli data <sub>` and, on a box with an engine and
  no `vike-cli`, `backtest data <sub>`. ⚠ It is a SURFACE move and could not have been anything else:
  all five open a `DataFusionHist` and `vike-cli` is DataFusion-FREE by construction (CI's
  `light-consumers` lane asserts it), so the implementation stays in `vike-backtest` and the CLI
  reaches it by SPAWNING. Each retired flag refuses BY NAME in BOTH spellings (`flag_given`, so
  `--fetch=…` is caught — the scripted form nobody is watching), and `--rm-series`' refusal says
  NOTHING WAS DELETED in as many words, because a cleanup script that starts exiting 2 must not read
  as "it may have partially run". `vike-cli data` gains the two subcommands that had no home —
  `export SPEC --out FILE` and `fetch-starter` — and `export`'s `--from`/`--to` are OPTIONAL and
  INDEPENDENT where `fetch`'s window is not, because a fetch with no window decides how much of
  somebody's rate limit to spend while an export bounds a slice already on disk. On the search side,
  `cmd/sweep.rs` is gone and `vike-cli backtest` absorbed `--rank-by` and grew `--optimizer`,
  `--euler-depth`, `--trials` and `--seed`; deleting the verb alone would have deleted the CAPABILITY,
  and growing it first surfaced a divergence that predates the merge — the server's `run_backtest`
  IGNORES a `[sweep]` table and reports ONE point while `--local` handed the same profile to an engine
  that ran the whole grid, same command line, two computations, nothing in the output saying which.
  Five knobs are `--local` only and each is refused BY NAME, because `Request::RunSweepProfile`
  carries no method selector and a silent downgrade to the grid is the precedence defect this release
  ended. The old spelling answers on the same `Exit::Usage` rung as an unknown verb but with the
  replacement in the message (`crate::RETIRED_COMMANDS`) — "unknown command 'sweep'" would tell a
  scripted caller that a verb which shipped for months never existed. A live defect closed on the way:
  `has_flag` is exact-token, so `--rm-series --yes --dry-run=1` IGNORED the `--dry-run` and RAN THE
  DELETE, on a command line written as a rehearsal.
- **binance, bybit and okx get the feeds/exec feature split, so a data daemon stops compiling the
  order plane.** Ruling 8: the module-level split between market data and the order plane already
  existed in all three CEX bridges, but the `[features]` tables did not — so a process that places no
  orders was linking `exec.rs`, the signed transport and the private user-data pump. One default-on
  `exec` feature gated at the `mod` line, nothing moved between files, every existing consumer's
  spelling unchanged and every default build byte-identical. `vike-aster` now takes `vike-binance`
  with `default-features = false` and forwards `vike-binance/exec` from its own `exec`: without that,
  features being additive, a signer-free aster build would have kept compiling the shared Binance
  family's signed rungs, which is the one crate best placed to exercise the new seam. ⚠ The
  `bridges-feeds` lane's `cargo tree` half for these three guards something NARROWER than
  hyperliquid's and aster's and the CI arm says so at length: they sign with HMAC-SHA256 through
  `vike_bridge_core::signer`, which rides `vike-bridge-core/full` that the FEEDS half needs anyway, so
  no crate leaves those trees at all and a crate-name grep would have passed in both configurations.
  The COMPILE half carries the weight instead, and genuinely does — a feeds module that reaches an
  exec module is now E0432. Two consts moved with the split and every call site was rewritten with
  nothing re-exported at the old path: `vike_binance::spot::VENUE` → `vike_binance::VENUE` and
  `vike_okx::transport::BROWSER_UA` → `vike_okx::BROWSER_UA`, both named by keyless kline code.

### Fixed
- **A subscriber that stops reading no longer parks a `vike-tradehub` thread forever.** The node's
  push writer had no write timeout, so a peer that kept its socket open and read nothing — the
  loopback peer is normally `sshd`, which does exactly that the moment its channel to a sleeping
  laptop stops draining — held its connection thread and a `MAX_CONNECTIONS` slot in `write_all`
  indefinitely; the heartbeat only ever caught a DEAD peer, whose socket eventually errors. The
  socket now carries `crates/vike-tradehub/src/server.rs`'s `PUSH_WRITE_TIMEOUT` (30 s, two
  heartbeats) from the moment a `Subscribe` turns the thread into a writer, a timed-out write
  closes the connection with a log line naming the bound, and the link-liveness suite proves it at
  150x scale. No wire change, no new setting.
- **The binance and aster perp depth lanes spoke SPOT's sequence rule, and reconnect-looped for forty
  days while every watchdog read green.** Measured on the recording box: `kind=depth/venue=binance/
  symbol=BTCUSDT.P` recorded 0.41–0.43 updates/s against the ~10/s a `@depth@100ms` stream carries —
  24x — with arrivals in PAIRS spaced 4.29–4.32 s at p50 across four windows at four times of day, and
  2,761 of every 3,600 seconds carrying no depth row at all while the binance TRADE tape on the same
  box in the same seconds was 84–99% busy. `crates/bridges/binance/src/family/depth.rs`'s
  `apply_depth_event` enforced binance's SPOT contiguity rule (`U > last_seq + 1` is a hole) on a
  USD-M FUTURES diff stream whose ids are non-contiguous by design, continuity being carried by `pu`,
  a field this workspace never read: the first post-seed diff applied by accident (it straddles
  `lastUpdateId`), the SECOND returned `DepthOutcome::Gap`, the session errored, and `run_depth_feed`
  slept `DEPTH_BACKOFF` and re-seeded — for ever, the two rows per cycle being the seed publish and
  the one diff that applied. The decoder now discriminates on the FRAME's own self-description (`pu`
  present means the futures grammar, absent means spot) rather than on a caller-supplied `is_perp`
  flag, so ONE edit fixes four venue×plane combinations — the same probe found aster speaks the
  futures grammar on BOTH its planes, so its SPOT depth lane had the identical defect. Replayed over
  the same live capture in the driver's own order, the old rule applies 1 frame and then gaps on
  binance perp, aster perp and aster spot alike, while the new one applies 295/267/194 with zero gaps
  and leaves the spot lane byte-identical. It was green for forty days because every fixture in the
  tree synthesised a contiguous diff as `U = last + 1` with no `pu` — the spot shape — including the
  shared conformance harness's binance `delta` builder, which now runs TWO rows whose futures half is
  RED against the old decoder. The disclosure half is why nobody knew: `FeedCtx::set_status` wrote a
  String into a mutex only the desktop's status bar reads and `depth_main` never called it at all,
  while `RecorderSink::stream_status` early-returned for every stream but `book`, so ~20,000 reconnect
  cycles a day reached neither the journal nor the store. Both are closed, `depth` markers now land in
  `kind=depth`, and the driver gains a rate-limited journal voice. ⚠ **Operationally this is a ~24x
  write amplification on a recording box** — depth goes from ~20 MB/day to ~500 MB/day and ~15.0M to
  ~373M rows/day — and `settings/recorder.toml`'s `max_merge_rows`/`target_mb` were sized against the
  BROKEN rate: re-check maintenance headroom after the first full day.
- **A market feed that recovered never said so, and that left one venue's live reconciliation
  suppressed for 42 hours.** Measured on a production box 2026-09-10: the live trading daemon
  suppressed bybit's reconcile leg once a minute for 42 hours — 2,516 occurrences, zero gaps — while
  the venue was perfectly healthy, with four ESTABLISHED sockets, ~2,100 core events a minute, the
  public API answering in 204 ms and zero faults logged in the entire window. The venue's
  authoritative wallet figure was frozen at one identical value across all 2,209 daemon summaries: the
  exchange's cash was not re-read once. Exposure was zero by LUCK from an unrelated subsystem — the
  Avellaneda maker was HOLDING because its break-even half-spread exceeded its configured maximum, so
  it posted no quote; had it been quoting, 42 hours of orders and fills would have gone unchecked
  against the venue. A deliberate restart cleared it and it re-latched twenty minutes later.
  `crates/vike-bridge-core/src/market_pump.rs`'s `run_market_feed_on` had exactly one status hook,
  `on_session_error`: a successful reconnect reset the backoff ladder — so the driver KNEW the session
  was healthy — and wrote nothing, while every CEX bridge wrote its healthy string ONCE at spawn, so
  one transient blip left `"… ws error (reconnecting): …"` standing for ever and
  `crates/vike-ops/src/reconcile_config.rs`'s `health_from_feed_status` reads that TEXT. Only a
  process restart cleared it, and that function's own doc had PREDICTED this in 2026-07 while its
  neighbouring claim — that `Error` is an unambiguous, currently-observed proof of a real fault — was
  false in its load-bearing word, because nothing kept it current. Patching bybit locally would have
  left eleven other bridges latchable, so this is the one coordinated change the shared-home rule
  asks for: `on_session_error: impl FnMut(&str)` becomes `on_session_status: impl
  FnMut(SessionStatus<'_>)`, WIDENED rather than added as an 8th argument so all ~26 call sites are a
  compile error and `match` exhaustiveness makes each venue's `Live` case a WRITTEN LINE in the diff.
  It fires on the first `FrameOutcome::Confirm` of a session rather than at connect-Ok: that covers
  both entry points (five production call sites drive `run_market_session` directly), reuses the one
  definition of "this session is working" the driver already trusts, is BOUNDED where the gate reads
  it, and manufactures no healthy reading during an accept-then-close storm. One healthy string per
  VENUE rather than per lane is a declared loss — `set_status` dedups on the TEXT, so per-lane
  spellings would turn every session boundary into a transition against the other lane's text, a
  journal line per reconnect into a file layer defaulting to `trace`.
- **Every DOM window after the first got an empty ladder, and the Connections tool called it live.**
  There are TWO doors into the market-data registry and only one of them attached. `MdSubscribe` goes
  through `crates/vike-datahub/src/server.rs`'s `run_market_writer`, which pushes
  `MdHub::attach_frames` per accepted key before its drain loop; `MdUpdate` — the door every window
  after the first takes, because `crates/vike-app-core/src/md_session.rs`'s `push_update` opens a
  fresh short-lived connection — went through `crates/vike-datahub/src/md/hub.rs`'s `MdHub::update`,
  which bumped the refcount, folded the depth and pushed nothing. Nothing else covered for it:
  `publish_tick` skips any entry whose `dirty` bit is clear and all three writers of that bit are
  SINK-side, so the adding session received no status and no snapshot until that venue's NEXT update —
  seconds to minutes on a quiet polymarket instrument, and the whole of a binance depth reconnect on
  the other side. The symptom is worse than blank: `MdSession::gapped` is populated only by an
  arriving `Status`, so a key that receives NOTHING is not gapped and `refresh_statuses` counts it
  among the live streams — an empty ladder while the tool reports `2/2 stream(s) live`, which is the
  exact failure `gapped` exists to prevent, reached through a door its doc did not know about.
  `update` now pushes `attach_frames` into the ADDING session's mailbox and reuses that function
  VERBATIM, so status-first, book-only-when-`Live` and tape-never-replayed remain properties of its
  STRUCTURE rather than of a second copy; the tempting alternative (`acquire` calling `mark_dirty()`)
  is not merely dearer but wrong in four ways and would BROADCAST one dirty key to every subscriber of
  it on behalf of a session that did not ask. It attaches per CHANGE and not per ACCEPTANCE,
  deduplicated by key, and that is a BOUND rather than tidiness: three requests re-naming the same
  already-held 64-key set would otherwise push 192 frames onto a 128-deep control lane and answer the
  peer `MdBye::ControlLaneOverflow`, making
  `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md`'s decision 2 — bounded by
  server constants, never by the request — false again on the one term it had to restore. Found by the
  wire CLIENT while its own half was being built, and reported against the server rather than fixed
  there.
- **The datahub trusted two wire fields, and one of them deleted series.** A client holding the
  Control key could send `DeleteSeries { selector, produced_by: Some(""), dry_run: false }` and the
  daemon would delete every series under that `kind=`/`venue=` subtree. The wire field is
  `Option<String>`, so a blank value is `Some("")` — not absent — and it stood down
  `delete_series_verb`'s `produced_by.is_none()` sweep gate; it then satisfied the provenance check
  VACUOUSLY, because `key_matches_prefix` is `starts_with` and every commit key starts with the empty
  string, so `RemovalPlan::verdict` found no foreign key, `execute_removal`'s re-check passed, and
  `delete_series_checked`'s re-assert under the series lock — the TOCTOU guard the whole verb turns
  on — passed for the same reason. Two guards and a re-check, one token.
  `vike_data::store_kind::resolve_produced_by` had refused exactly that spelling since it was written
  and had ONE caller in the tree: the ENGINE's local `data rm` arm. The cure is the WHOLE resolver
  rather than a re-implemented blank test — re-exported as
  `vike_datahub_client::proto::resolve_produced_by` beside the removal vocabulary that is there for
  exactly this reason, and imported from there by the server, so one grep shows both ends of the wire
  sharing one definition of a valid producer filter. It is called AT THE DOOR, before the plan and
  before one `series_commits` read, and unconditionally: refusing a DRY RUN matters because that arm
  returns the plan WITHOUT consulting `verdict()`, so a blank dry run used to render
  `provenance: SATISFIED` — and the MCP surface's `preview_token` binds to exactly that plan. Second
  hole, same shape: of a subscription spec's five fields, `symbol` was held by nothing but the
  post-auth 64 MiB frame ceiling, so an over-length one was ACCEPTED, cloned into the registry, and
  woke the reconciler into calling `subscribe_depth` on the REAL venue, after which `attach_frames`
  emitted a `Status` carrying the string verbatim far over `MD_CTRL_FRAME_CEILING_BYTES` — a TERM in
  `MD_MAILBOX_BYTES`' compile-time assertion, and therefore in the co-hosting budget that
  `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` states and rests on.
  `MD_MAX_SYMBOL_BYTES = 96` sits inside a derived range and the range is the argument: a
  FLOOR of 78, because a polymarket CLOB token id is a uint256 spelled in decimal and 2^256−1 is
  exactly 78 digits — a maximum by construction, not a measurement — and a CEILING of 109 from the
  widest framed empty-symbol `Status` the roster can produce. Authentication says WHO may send a verb;
  it says nothing about WHAT they may send, and nothing else was looking.
- **Four ways `backtest`'s search flags gave a DIFFERENT ANSWER instead of a refusal.** The
  hand-written ladder in `crates/vike-backtest/src/backtest_cli.rs` had three arms, each parsing its
  own flags INSIDE the branch that used them, so a flag belonging to a branch not taken was never read
  and never validated — one cause with four faces, every one of them exiting 0: `--optimizer tpe
  --search bogus` ran tpe; `--search euler --trials 0` ignored a `0` that the tpe arm treats as fatal;
  `--search grid --euler-depth abc` was accepted and discarded; and `--optimizer=tpe` RAN THE GRID,
  because `crates/vike-analytics/src/binutil.rs`'s `arg` matched an exact token only, which is the
  worst of the four since it is not a refusal but a full ranked report with no diagnostic — and
  `--rank-by=return` ranked by Sharpe and `--store=DIR` read a different store on the same path. A
  fifth, found while reading: the whole ladder sat inside `if profile.is_sweep()`, so `backtest
  run.toml --optimizer tpe --trials 500` ran ONE ordinary backtest and exited 0. `METHOD_FLAGS` now
  says which method owns each knob and a knob handed to a method that does not own it is a REFUSAL, so
  `--trials 0` answers uniformly across methods instead of being fatal on one path and ignored on two;
  `--search` retires as an ERROR rather than an alias, because both spellings can be given with
  DIFFERENT values and every resolution of `--optimizer tpe --search euler` is a guess; a value-less
  trailing `--optimizer` is refused rather than read as absent (`required_value`), which is the
  spelling `--optimizer $METHOD` writes when `METHOD` is unset; and argv is triaged BEFORE any I/O,
  since `DataFusionHist::open` `create_dir_all`s its root and a mistyped flag used to mint an empty
  store on the way to its own error. The same widening audit closed a blank `--produced-by` in
  `resolve_produced_by`, where an empty prefix matched EVERYTHING and stood down both the provenance
  assertion and the rule requiring provenance before a wildcard delete.
- **The two shipped deploy templates disagreed about where the hist store lives.**
  `deploy/vike-backtest.service` shipped `VIKE_HIST_STORE=<root>/data/hist` while
  `deploy/vike-datahub.service` and `deploy/vike-datahub-record.service` shipped
  `VIKE_DATAHUB_STORE=<root>/market_data/hist`. `market_data/hist` is the authoritative spelling —
  `crates/vike-model/src/state_path.rs`'s `PROJECT_DATA_DIR` + `HIST_SUBDIR`, which
  `crates/vike-ops/tests/deploy_layout_gate.rs`'s `STORE_REL` pins — so an operator following both
  templates and substituting ONE real project root, exactly as the `sed` at the top of each file tells
  them to, got a compute daemon opening a directory the data daemon neither fills nor serves. The
  failure is the quiet kind, and the unit warned about it in its own voice six lines above the defect:
  `data/` holds no `kind=` partitions, so every run loads zero bars and reports a clean, empty, WRONG
  answer rather than refusing. No box in service was affected — only the TEMPLATE pair a NEW
  deployment is copied from, which is why nothing on a running box could reveal it. The gate that
  should have caught it was running over a roster that had silently stopped being complete:
  `every_shipped_store_root_is_one_spelling` iterated a hand-written list of exactly the three datahub
  units and read only `VIKE_DATAHUB_STORE`, so both backtest units were compared to nothing. It now
  iterates `DAEMON_UNITS` — already held exhaustive against the `deploy/` directory itself — and checks
  every unit that NAMES a store through either variable in the new `STORE_ENV_KEYS`, so a future
  store-naming unit joins the check by existing. A derived roster fails differently from a listed one,
  in that it can go silently EMPTY, so the rule additionally proves both spellings were exercised by a
  real unit and that no exemption row stands over a unit naming no store.
- **The backtest daemon's default port was the LIVE TRADING DAEMON's.** Ruling 7 picked
  `127.0.0.1:7879` for `backtest --addr`, and that port is taken:
  `crates/vike-config/src/config.rs`'s `node_addr` doc says the tradehub binds it on its own box, and
  a listening-socket check on a production box found the daemon that signs orders holding it. It was
  caught by a review of an unrelated PR whose `study` client had copied the same number and would
  therefore have dialled the trading socket by default. The numbers are now datahub 7878, tradehub
  7879, backtest daemon 7880 (`DEFAULT_BACKTEST_ADDR`), with the collision and its evidence written
  down at the constant — and pinned there by an `assert_ne!` against 7879 — so the next reader does
  not re-pick it.
- **Five shipped strings named the recording box, and only a tag would have found out.**
  `SERIES_CADENCE`'s `measured:` fields are `&'static str` DATA, not comments, so a box name in one of
  them was compiled into `vike-backend`'s rodata — and `scripts/refuse_box_paths.sh` greps the raw
  bytes of every published asset. That guard runs in `release.yml`'s `build` and `windows` jobs and
  NOWHERE ELSE, so the first tag cut after the recorder work would have built for ~15 minutes,
  refused, and published nothing; and because a tag-push run executes the sources AT THE TAG, a fix
  landing afterwards could not be reached by re-running it — the tag would have had to be re-cut.
  ⚠ **No PR could have caught this**, by construction: `scripts/publish_mirror.sh` REDACTS the token
  out of every text file, `.rs` included, BEFORE it runs the same token list as its FORBID scan, so
  the mirror gate sees a clean tree; `api-docs` documents that already-redacted copy; and
  `compile_time_path_gate` polices `env!("CARGO_MANIFEST_DIR")`-class embeds rather than hand-written
  literals. The binary is the only place the token survives, and only the release looks at a binary.
  Comments are deliberately left alone — the lexer discards them, so they reach no asset, and a
  sentence recording that a number was MEASURED rather than guessed is the thing the next reader
  needs. Only the five strings that SHIP changed, and every backticked path-and-symbol citation inside
  them is byte-identical.

## [0.1.23] - 2026-09-10

### Changed
- **The release hand-off no longer goes through GitHub.** `build`/`gui`/`windows` hand their assets
  to `publish` through a per-run directory on the CI box's nvme (the workflow's hand-off step
  names it) instead of `actions/upload-artifact`, which is what refused
  v0.1.22 at publish with every binary built. `packaging_gate` pins the four jobs to one `runs-on`
  label, and `publish` refuses a lane whose hand-off came from another host.
- **CI uploads nothing to GitHub's artifact storage any more.** The `api-docs` job attached its ~43 MB
  rustdoc bundle on most code PRs at seven days' retention — 140 copies / 6.03 GB on 2026-09-10, the
  whole of the org's quota, which is what refused v0.1.22's release uploads with every binary built —
  and nobody ever downloaded one. The job keeps its scan and its verdict; the built tree stays on the
  runner. `docs/ops/api-reference.md` carries the delivery question, unchanged.

### Note
- **v0.1.22 was tagged and never published.** Every asset built; GitHub refused the artifact uploads
  its release workflow still used ("Artifact storage quota has been hit"). This release supersedes it
  from the same tree plus the two changes above, and is the first to ship through the on-box
  hand-off. The v0.1.22 tag stays as a record; no Release and no deploy ever existed for it.

## [0.1.22] - 2026-09-10

### Changed
- **⚠ THE BACKEND IS `vike-backend`, THE DESKTOP IS `vike-desktop`, AND THE VERBS LOST THEIR
  `vike-` PREFIX** — the rename signed off in #1725 and landed as #1726, #1727 and #1729.
  - `vike` → `vike-backend` (the multicall; the bin follows the PACKAGE name, `crates/vike/Cargo.toml`
    declares no `[[bin]]`). Verbs: `vike-backend trade` (was `vike vike-tradehub`), `recorder`,
    `datahub`, `study`, `report` (was `tearsheet`); `backtest` and `vike-cli` are unchanged.
  - `vike-cli node …` → `vike-cli backend …`.
  - `vike-app` → `vike-desktop`, and the `fat` build is DELETED: the desktop signs nothing, mounts no
    venue and opens no venue socket — it works only against a running backend. The GUI release
    assets are `vike-desktop` / `vike-desktop.exe` (were `vike-app-fat` / `vike-app.exe`).
  - The container's links are the verbs (`trade`, `report`, …) and its entrypoint execs `trade`.
- **⚠ DEPLOY IS A THREE-STEP MIGRATION and this release is step two.** The helper accepts either
  backend name for one cycle, installs `bin/vike-backend` BESIDE `bin/vike`, and reports SKEW —
  "INSTALLED (NOT IN SERVICE)" — until each unit's `ExecStart=` is flipped to `bin/vike-backend
  <verb>`, `daemon-reload`ed and restarted, after which the deploy is re-run. `bin/vike` is removed
  by hand as the LAST step, and the next cycle retires the dual-name acceptance.
  `docs/ops/upgrading.md` carries the sequence.
- The multicall's feature parity gate has its second spelling back as a declared table
  (`SHIPPED_TOOL_FEATURES`) now that the per-package release build is gone; the image gate's
  cargo-side question narrows to the one binary every link points at.

### Removed
- The pmxt ClickHouse sink (`pmxt_backfill`) — a write path with no dedup key (#1724).

### Docs
- `docs/superpowers/specs/`: the rename design (#1725); the datahub market-data wire design carrying
  owner rulings 7–17 and the `Optimizer` trait design derived from grid/euler/TPE (#1728) — all
  awaiting sign-off, nothing built.

## [0.1.21] - 2026-09-09

### Changed
- **⚠ THE RELEASE SHIPS ONE BACKEND BINARY, NOT SEVEN AND THEIR SUM.** `vike` — the multicall —
  already contained every per-tool binary, so attaching both shipped the same program twice: **629 MB
  of per-tool binaries beside the multicall's 148**, measured on v0.1.20, uploaded to the release,
  downloaded again by every deploy so `sha256sum -c` could verify the manifest it came with, and
  re-published to the public mirror. `ASSETS` is now `(vike-cli vike)`.
  What is deliberately KEPT: the per-package `cargo auditable build` (it is `multicall_gate`'s
  comparison half — the only thing that can see a feature DROPPED from the multicall rather than one
  that fails to compile); `vike-cli` (9.6 MB, what `cargo binstall` fetches, and named on four units'
  `ExecStartPre=`).
  ⚠ **Every unit and every install path names the dispatcher now** — `ExecStart=<root>/bin/vike
  <tool>`, a SUBCOMMAND rather than a symlink, so there is no `203/EXEC` class to inherit. This is a
  reversal of #1702's reversal and the reasons differ: that one was right about LINKS, and the
  multicall never needed them. `deploy/sbin/vike-trader-ci-deploy` installs two files and REMOVES
  the three it used to install — its old loop reported an absent asset as "left unchanged", which
  from this release means a stale binary beside a new `vike` with nothing saying so.
  ⚠ Two guards were already lying before this touched them, both from #1716: `deploy.yml` required
  `vike-tradehub` while the helper had moved to `[ -f vike ]` (it would have failed EVERY deploy from
  this release, three steps before the helper it guards), and `run_mutations.sh`'s A22 anchor was
  dead for the same reason. Both fixed. `deploy/vike-tradehub.service` also promised "this DEFAULT
  build contains NO Telegram control channel" — the release builds `vike-tradehub/telegram`, so the
  path IS linked and what keeps it inert is the four runtime gates; the unit says that instead.
- **`vike-cli` finds the backtest engine inside the multicall**, so a release need not ship a
  standalone `backtest` for `--local` to work.

### Added
- **⚠ FXCM IS IN THE SHIPPED BINARY: the ForexConnect SDK is opened at RUNTIME.**
  `crates/bridges/fxcm/build.rs` emitted `cargo:rustc-link-lib=ForexConnect`, so any binary built
  with `--features fxcm` carried a hard `DT_NEEDED libForexConnect.so` and **did not reach `main` on
  a box without the libraries staged** — exit 127. Hence a SECOND 148 MB daemon asset,
  `vike-tradehub-fxcm`, for the boxes that trade FX.
  The C++ boundary is its own shared object now: `libfcshim.so` links the SDK the ordinary way and
  `crates/bridges/fxcm/src/loader.rs` opens it at runtime, resolving ten `extern "C"` entry points by
  name. **No Rust binary references a ForexConnect symbol.** So `crates/vike/Cargo.toml`'s `full`
  forwards `vike-tradehub/fxcm`, the one shipped `vike` carries the venue and starts exactly where it
  started before, the twin asset is GONE, and the release attaches the ~100 KB shim instead.
  ⚠ **The obvious smaller change — dlopen the SDK and dlsym `CO2GTransport::createSession` — was
  planned and is WRONG**, measured rather than reasoned: the shim needs FIVE SDK symbols, because it
  DERIVES from two SDK interfaces whose out-of-line constructors, virtual destructor and typeinfo
  live in the library. Defining them ourselves would put a second `typeinfo for IAddRef` in the
  process — an ODR violation that stays quiet until something compares type identity across the
  boundary. `loader.rs`'s header carries the `nm -u -C` output.
  ⚠ **The compile-time stub is gone, which is a coverage win**: every method in `sys.rs` existed
  twice behind `#[cfg(fcsdk)]`, so CI compiled the stub and never the FFI. The same code compiles on
  every box now, and a MISSING symbol is a diagnostic at load rather than a crash on the call that
  needed it. `sdk_linked()` → **`sdk_available()`** (it answers about THIS box, not the build box),
  with `sdk_unavailable_reason()` naming every path the loader tried.
  ⚠ **Operator step on a box that trades FXCM:** `just fxcm-package <root> <shim>` — the recipe takes
  a second argument, because the libraries alone are no longer sufficient. Omitting it packages them,
  says loudly that FXCM stays paper, and exits 0. `ldd <root>/bin/vike` now says nothing about FXCM
  either way; `ldd <root>/lib/libfcshim.so` is the check that means something.

### Fixed
- **`mirror-publish.yml` blamed the token for a missing tool, on its first real run.** The workflow
  shipped in 0.1.20 and had never executed; `gh` is not installed on the the CI box runners — a fact
  `release-image.yml` and `release.yml` had both MEASURED and written down — so `gh api` exited
  non-zero and the job reported `MIRROR_TOKEN cannot read … check its scope`. The token was correct.
  The prerequisite is its own assertion now, ahead of the question that depends on it. A
  `workflow_dispatch` also runs the workflow from `main` and `publish_mirror.sh` from the TAG, so the
  two can disagree; the header says so.

### Removed
- **The scoop bucket — added in 0.1.20, gone four days later. It was a third spelling of one
  install.** What it installed was `vike-cli.exe` from the release, which `cargo binstall` resolves
  through `[package.metadata.binstall.overrides]` and which a `curl.exe` of
  `releases/latest/download/vike-cli.exe` fetches directly; the bucket added a manifest pointing at
  that same asset and nothing else. Nobody asked for it — it was built because the Windows CLI asset
  had made it possible — and it carried a defect neither of the other two has: the mirror is one
  ORPHAN commit per publish and scoop refreshes a bucket with a plain `git pull`, which refuses
  unrelated histories, so `scoop update` could not advance it and `README.md` had to teach a
  remove-and-re-add procedure instead. The escape was a bucket repository of its own, whose CODE
  shipped in 0.1.20 and whose REPOSITORY nobody created. Deleted with it: the renderer, its
  template, the bucket publisher, `publish_mirror.sh`'s `--bucket` / `--bucket-in-mirror` /
  `--sums` flags, `mirror-publish.yml`'s second write target and the second scope on `MIRROR_TOKEN`,
  eight `packaging_gate` cases and two `publish_mirror_gate` cases. **No shipped artifact lost
  coverage and no install path was removed** — the two that remain are the two that always worked.
  ⚠ If you have it installed: `scoop uninstall vike-cli && scoop bucket rm vike`, then use either
  line in `README.md`; the binary is byte-identical. `docs/decisions/0037` carries the removal, what
  was learned building it (the binstall override pair, which is NOT scoop's and stays), and the one
  condition that would bring a bucket back — somebody who is not us asking for one.

## [0.1.20] - 2026-09-08

### Added
- **A gate over the multicall's feature parity, which three files asserted and nothing compared.**
  The release builds the seven tools twice — once per package with an explicit `--features` list,
  once as `vike --features full` — and `release.yml`, `crates/vike/Cargo.toml` and
  `scripts/ci_feature_suite.sh` each said those two spellings "must stay equal" while pointing at
  each other. ⚠ No lane could see a drift: the `multicall` lane compiles `-p vike --features full`,
  so a tool row that fails to COMPILE is caught, but a feature merely DROPPED compiles fine — a
  `vike-recorder` without `polymarket` records nothing for that venue and looks exactly like an
  unconfigured one, while every per-crate lane stays green because each crate is still built with its
  own features elsewhere. `multicall_gate.rs` now resolves `full` transitively and compares it to the
  workflow's per-package list. They agreed on all ten forwards, which is what made this a cheap
  moment to gate rather than a bug report; proven able to FAIL by dropping
  `vike-recorder/polymarket` on a throwaway branch and watching it name that feature.
- **The Windows CLI installs from the release: a `cargo binstall` override pair and a scoop bucket.**
  `release.yml` already cross-built `vike-cli.exe`; nothing installed it. Now
  `[package.metadata.binstall.overrides]` carries `x86_64-pc-windows-gnu` AND
  `x86_64-pc-windows-msvc`: binstall resolves EVERY triple `detect_targets` returns, and on an msvc
  host that list already carries the gnu fallback (`detect_alternative_targets`), so the msvc row is
  not what makes the install possible — it makes the FIRST probe the right one instead of ten
  default-template 404s. The public mirror is now also a scoop bucket:
  `scoop bucket add vike https://github.com/vike-io/vike-trader && scoop install vike-cli`.
  ⚠ The scoop manifest is RENDERED at publish time, never committed: it carries a concrete version,
  url and digest, and the DIGEST alone settles it — it belongs to a binary the release runner has
  not built yet. (This read "this tree has none of the three (every crate is `0.1.0`, the tag is the
  version…)". The version half stopped being true later in the same release — see *The version is
  the tag*, below — while the digest half is permanent.)
  `scripts/scoop/vike-cli.json.tmpl` is the committed
  template; `scripts/render_scoop_manifest.sh` fills it from the tag and the release's own
  `SHA256SUMS`, and `scripts/publish_mirror.sh` writes the result to `bucket/vike-cli.json` in the
  mirror tree BEFORE it pushes — which also moved every `gh`/slug/manifest refusal ahead of the
  source push, so they now fail while nothing is public.
  ⚠ **Both of those files were DELETED on 2026-09-09 with the bucket itself** — see *Removed* under
  *Unreleased*. This entry is left as it was written because it records what 0.1.20 shipped; the
  refusal-ordering half of the last sentence is the part that outlived it and is still true.
  ⚠ **`scoop update` cannot advance this bucket, and the install docs say so rather than promising
  otherwise.** The mirror is one ORPHAN commit per publish (`git init` + `git push -f`, so no
  earlier tree can hand back a file later withheld) and scoop updates a bucket with a plain
  `git pull -q`, which refuses unrelated histories; a later release is installed by removing the
  bucket and adding it again. `docs/decisions/0037` records the scoop deferral as LIFTED, that
  update gap as a deferral of its own (a bucket repository with real history), and Homebrew as
  still waiting on a macOS runner that does not exist.
- **⚠ BREAKING BY DEFAULT — a live `vike-tradehub` mount now has an automatic stop it was not
  asked for: the CONNECTION-state dead-man.** `policy.toml`'s new `link_deadman_grace_ms` is **ON
  when absent** (`vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS`); `0` is the only off, and the armed
  range is `MIN_LINK_DEADMAN_GRACE_MS`..=`MAX_LINK_DEADMAN_GRACE_MS`, refused at load by name and
  never clamped. When a venue's own bridge reports its market-data link `Disconnected` for longer
  than the grace, **that venue's** resting orders are cancelled (`OrderIntent::MassCancel` scoped to
  it, so a bybit socket death leaves a binance book alone) and — under the SHARED `deadman_action` —
  HALT engages: `Halted` on every engine plus the one cross-process sentinel file, which is
  process-wide because that file is.
  **It is not the silence switch with a new number.** `deadman_timeout_ms` counts INGEST, cannot
  tell a closed market from a dead socket, and stays OPT-IN for that reason; this one reads the
  `FeedStatus` the bridges DISCLOSE, and `Stale` — which is exactly what a weekend or an evening
  close looks like — never counts in either direction. That is what lets it have an armed default.
  ⚠ **It does not cover every venue, and cannot be made to from the file.** Where it arms is a
  per-venue table read from each bridge's emitter (`vike_model::link_deadman_default`): every FX and
  equities venue is OFF because its market has sessions, and most roster venues disclose no
  disconnect at all and are declared residuals. ⚠⚠ **And a second cut:** a venue arms only if this
  daemon subscribes the lane that carries the disconnect. As first shipped that cut was the binding
  one — binance/aster/bybit/okx disclosed a disconnect from their L2 DOM depth feed alone, which
  `vike-tradehub` does not subscribe, so a default (non-`polymarket`) build armed NOTHING and
  constructed no switch. **Closed before release** by the Changed entry below (the CEX tick pumps
  now disclose); the cut itself remains a wiring fact no `policy.toml` line can reach, in either
  direction. The mount logs ONE line per mounted venue at
  startup saying armed-or-why-not, and builds no switch at all when none is armed. `vike-app` and
  the headless IBKR mount stay unwired, as they were for the silence switch.
  `docs/decisions/0038-the-dead-man-observes-the-connection-not-silence.md` is the ruling;
  `docs/ops/kill-switches.md` carries the operator row, the residuals and the two gaps.

### Changed
- **`vike-cli` resolves the datahub node keys from the credential store, not only the environment.**
  It read `VIKE_DATAHUB_OBSERVE_KEY`/`VIKE_DATAHUB_CONTROL_KEY` from `std::env::vars()` and nowhere
  else, so a pair sitting in `<project>/settings/secrets.env` — where its own refusal text sends an
  operator — did nothing. ⚠ The same defect had already been found and fixed one key pair over: the
  module doc records that both TRADEHUB node keys "used to read `std::env::var` and nothing else, so
  a correctly-configured box answered 'nothing to do' and exited", and the datahub pair was then
  written in that shape with a comment claiming it worked "exactly as `node_keyring` does". The worst
  part was the refusal naming the wrong remedy. `datahub_keyring` now mirrors `node_keyring` — env
  first, store second, lazily, for the five arms that can dial a datahub — and `credential_store` is
  factored out so the read has ONE spelling. Measured against a keyed datahub: keys in the store
  alone gave "no node keys were supplied"; the same keys resolve now.
  ⚠ This WIDENS which arms open the venue-key file. The rule was "a provenance or backtest command
  has no business opening the file that holds every venue key on the box"; it is now "an arm that can
  dial a node may open the store", and `config`/`secrets`/`init`/`indicators` still never do. The
  narrower version — read the store only for a REMOTE invocation — was refused on measurement:
  `cmd::data`'s address resolves to a default whether or not `--addr` was passed, so "did this go
  remote" is not answerable from argv.
- **The deploy installs the tools a box could not previously run.** `deploy/sbin/vike-trader-ci-deploy`
  installed `vike-tradehub` and `vike-cli` and nothing else, so everything else on the box arrived by
  hand — which is how its `vike-recorder` came to be months old with nothing saying so. It now also
  installs `backtest`, `vike-study` and `tearsheet` as REAL per-tool binaries, selected by the rule
  *no systemd unit names it*, so installing them restarts nothing. `tearsheet` is the one worth
  naming: it reads the live daemon's journal and prints the same metrics a backtest over those fills
  would, on a box that could not previously render its own performance. Each goes down through a temp
  file and `rename(2)` — writing over a RUNNING executable fails with `ETXTBSY`, and a human's
  `backtest` can be mid-deploy. A missing asset is REPORTED per name, never fatal: the helper is
  installed out of band and will meet older releases. `status` now RUNS each tool's `--version`
  rather than listing names, so a tool that cannot execute is visible instead of implied.
  ⚠ Briefly this installed the `vike` multicall plus symlinks instead (631 MB of binaries against
  146 MB) and that was REVERSED on measurement: the deployment disk had 7 TB free, so the saving was
  0.009% of it, the deploy downloads every asset either way, and a dangling link is
  `status=203/EXEC` on a tag with no reviewer. The container image still ships the multicall, where
  the size genuinely matters because every user pulls it.
- **The version is the tag.** Nineteen releases shipped as `0.1.0`: all 64 member manifests spelled
  `version = "0.1.0"`, `[workspace.package]` carried no version, and `just release` cut a tag while
  bumping nothing. ⚠ The costs were user-facing, not cosmetic — `cargo binstall` had to resolve
  `releases/latest/download/` because `{ version }` rendered ONE string for every tag, so a user
  could install the **newest release and no other**, and a re-run reported "up to date" whatever the
  tag (`--force` was the way round it); `scoop list` said `0.1.19` while the binary it installed said
  `0.1.0`. The root `[workspace.package]` now carries the one `version`, every member takes
  `version.workspace = true` — the version is written **once** where it was written 64 times — and
  `just release X` rewrites and commits that line, refreshes `Cargo.lock`, and only then tags `vX`,
  so the tag cannot point at a tree that disagrees with it. `binstall` moved to
  `releases/download/v{ version }/`, which is exactly what `crates/vike-cli/Cargo.toml` had promised
  for "the day the release recipe bumps this manifest's version to the tag". A build BETWEEN releases
  reports the last released version; the identity that moves is the git commit
  `vike_buildinfo::version_line` already prints beside it. Three manifests keep their own versions
  because they are not workspace members (`crates/bridges/ctrader/protogen`, `fuzz`, the vendored
  `ibapi`), and `Cargo.lock` was updated by package NAME because five third-party crates genuinely
  are `0.1.0`.

- **The connection-state dead-man now REACHES the venues a default `vike-tradehub` daemon mounts.**
  It shipped ON by default and armed nothing on a non-`polymarket` build: binance/aster/bybit/okx
  disclosed a dead link from their L2 DOM depth feed alone, and the daemon's CEX arm subscribes the
  kline lane and the quote/trade/book tick pump — not that one. The pumps now disclose their own
  transport state (`crates/bridges/binance/src/family/depth.rs`'s `disclose_link` and its bybit/okx
  twins) straight onto the core tick lane they already write ticks to: one `Disconnected` per
  outage, one `Live` on the FIRST FRAME back rather than at connect, so a venue that accepts a
  socket and drops it in a loop reads as one outage instead of resetting the grace forever.
  Chosen over subscribing the DOM feed because that would open a SECOND socket to the same
  `@depth`-class stream on the same host, maintain a second copy of the same book and add a
  five-minutely weight-50 REST re-seed — for a signal the first socket already has, delivered into
  a sink verb that is a no-op in this daemon. Zero extra bytes on the wire. The per-venue table is
  still the authority (every FX and equities venue stays OFF — a close is not an outage), each CEX
  row now cites both emitters, and a gate panics if the mount ever stops subscribing a disclosing
  lane. `docs/decisions/0038-…` records the closure under its own "what would reopen this" clause;
  the residual is that no production link death has yet been watched tripping it.

### Removed
- **The `poly-l2-monitor` files left this repository for the one they describe.** Five files — a
  health check, its unit, its timer and two harnesses — watched a SEPARATE project's ClickHouse tape
  (`poly-l2-recorder`) while living here. ⚠ The tree had already NOTICED: two gates carried declared
  exemption rows saying so in as many words ("a `Type=oneshot` health check for the SEPARATE
  closed-repo poly-l2-recorder"; "it belongs to that repo's deployment, not this one's"). That is the
  shape worth remembering — an exemption whose REASON is "this belongs to another project" is a
  request to MOVE the file, not a reason to exempt it, and writing the exemption froze a correct
  diagnosis into infrastructure. They are in that repository now; both tables lost their rows and
  gained a note saying why. `vike-notify` deliberately stayed — it is shared alerting infrastructure
  three other units hang off. The three prose citations of the moved files are KEPT, as
  `DEAD_PATH_EXCEPTIONS` rather than deletions: they are the evidence for an incident write-up, and
  the citation gate cannot follow a path across a repository boundary.

### Fixed
- **The bybit and okx market-data pumps dialled with NO connect bound**, so a black-holed route
  pinned their feed thread for the OS's own SYN ladder (~127 s on Linux defaults) with the stop
  flag already raised behind it. The binance/aster twin of the same pump had this fixed on
  2026-08-08 and these two were missed; `crates/vike-ops/tests/feed_stop_windows_gate.rs` found
  them the moment those files named a feed driver. Both now dial through
  `vike_bridge_core::ws_proxy::connect_ws` under `pump_spec::CONNECT_10S`, the same bounded arm
  every other feed takes. It matters more than it did: this is now the lane the connection-state
  dead-man hears a bybit/okx link die on, and 127 s of unstoppable dial is longer than that
  switch's own default grace.
- **The absent-`deadman_timeout_ms` warning no longer says "this live mount has NO automatic stop".**
  That sentence became false the moment the switch above shipped armed by default, and an operator
  acting on it would either disable a real protection or write a key that halts an FX mount every
  Friday. The message now names WHICH switch is off, says the other is on, and points at the
  per-venue lines beside it; a test asserts the retired sentence cannot come back.

## [0.1.19] - 2026-09-05

### Added
- **`vike-cli secrets set KEY` — the one credential writer the CLI is allowed to have.** The value
  comes from **stdin** or from a **named environment variable** (`--from-env NAME`) and NEVER from
  the command line: `secrets set KEY VALUE` is a usage error, because argv lands in shell history
  and in `ps` output for every user on the box, and the refusal deliberately echoes nothing the
  operator typed. It is a SECOND CALL SITE of `vike_secrets::save_credentials`, the workspace's one
  byte-preserving atomic upsert — never a second writer — so every other line, comment, blank and
  their order survive verbatim. The key is validated against `vike_model::credential_keys` and an
  unknown name is refused BY NAME with the nearest real ones; an ABSENT store is refused too,
  naming `vike-cli secrets template`, because creating the file stays the operator's decision. Each
  write appends one `change_journal` `credential_write` record (key NAMES only, never a value).
  ⚠ The MCP surface stays credential-free and none of this is reachable from it.
  `docs/decisions/0036` fixed this exact shape BEFORE the writer existed and now records that it
  exists; `crates/vike-ops/tests/credential_writer_gate.rs` pins the set of files in the whole tree
  that may call a credential writer, with the reason each may.
  Four narrowings landed in review, each now covered by a test: **no unrecognised token on `set` is
  quoted back at all** (a value starting with `-`, which base64url allows, used to be printed
  verbatim to stderr by the generic unknown-option arm); **`--file` is REFUSED on `set`** and stays
  an inspection flag on `list`/`path`/`template`, because it aimed a write at any path the operator
  named — `$VIKE_SETTINGS_DIR` is how a scripted run targets another project; **a multi-line value
  is refused** rather than written (it truncated the credential at the newline and turned the
  remainder into a second `KEY=VALUE` line for a key nobody named); and **a `KEY__LABEL` account
  name is refused without suggesting the unlabelled key**, which is a different account's live
  credential.
- **`vike-cli.exe` is now a Windows release asset.** The Windows job cross-built the GUI and nothing
  else, so a Windows user got `vike-app.exe` and no terminal surface at all — no `config check`, no
  `secrets path`, no `backtest`, and no way to run the agent-facing `mcp` server. ⚠ The standalone
  `backtest` engine is still a **Linux/container asset only**: `backtest --local`, `sweep --local`
  and the whole `data` verb therefore need an engine a Windows user supplies themselves, and the
  missing-engine message says so and names the command that builds one.
- **`vike-cli data fetch` and `vike-cli data seed-demo`** — market data into the hist store without
  knowing a second binary's name. Both drive the standalone `backtest` engine as a child process;
  the CLI itself stays DataFusion-free.
- **`vike-cli backtest --local` and `vike-cli sweep --local`** — the same run on this machine, with
  no `vike-datahub` to talk to, by driving that same engine. `--preset`/`--script` still apply
  client-side in both modes, so a local run is a rehearsal for a remote one.
- **`--json` on `secrets list` and `init --dry-run`**, the two machine-readable surfaces that were
  missing. `secrets list --json` prints key NAMES and their venue, never a value.
- **`cargo binstall --git https://github.com/vike-io/vike-trader vike-cli` installs the released
  CLI without a toolchain.** `crates/vike-cli/Cargo.toml` gained a `repository` field naming the
  PUBLIC mirror and a `[package.metadata.binstall]` section spelled against what `release.yml`
  ships — a bare Linux binary, no archive, no platform suffix — and read from cargo-binstall's own
  documentation rather than from memory. Linux only: there is no `vike-cli.exe` and no macOS
  build. It points at the mirror's LATEST release, because a release bumps no crate version and a
  `v{ version }` URL would never resolve. `crates/vike-ops/tests/packaging_gate.rs` renders the
  template itself and refuses one naming an asset no `SHA256SUMS.part-*` line carries.
- **The public mirror's release now carries the binaries.** `scripts/publish_mirror.sh` copied
  only the docs-data JSON from the private release, so a public `cargo binstall` would have
  404'd on its first run. It now downloads `SHA256SUMS`, then every asset it names, verifies each
  against it, and uploads them all plus the manifest — derived from the manifest, never a second
  list, the rule `release.yml`'s attach loop already follows. The docs-data handling is unchanged.
  Nothing becomes public until the owner runs the publish for a tag.

### Changed
- **⚠ `vike-cli`'s exit code is now a ladder rather than a boolean, which is a behaviour change for
  anything wrapping it.** `0` (success) and `1` (ran and failed) keep their exact meanings, so a
  script written against the old two-value behaviour still reads correctly — but failures that used
  to exit `1` now exit `2` when the command line was wrong (an unknown verb or flag, a missing
  required flag, no subcommand at all) and `3` when a service could not be reached (no datahub, no
  node, a refused or timed-out connection). The point is the distinction a wrapper needs: `2` is
  FIXED, `3` is WAITED on, `1` is the ordinary failure. `crates/vike-cli/src/exit.rs` is the
  authority; `4` and `5` appear in that enum and are RESERVED — nothing produces them yet, and a
  script may not branch on them.
- **The pinned Rust toolchain moved 1.96.0 → 1.97.1.** `rust-toolchain.toml` is the only thing
  making the four build environments agree — the Windows dev box, CI (the CI box), the latency runner
  (the latency box) and the CI box's test clone do not share a default toolchain — so the pin is the whole
  mechanism, and no box was touched individually.
- **The workspace moved to Rust edition 2024.** 64 manifests; the vendored `ibapi` copy and the
  workspace-excluded `ctrader/protogen` recipe deliberately stay on 2021 — the first is
  byte-compared by its drift gate. The migration was cheap for a reason worth knowing: the
  workspace's `unsafe_code = "forbid"` had already eliminated both of the edition's headline
  breakages (zero `unsafe fn`, zero `static mut`). What it DID force is the useful part: every
  `std::env::set_var` left the tests — 39 sites across 7 files — because the edition makes it an
  `unsafe fn` and the forbid cannot be lifted. Each test now drives a pure core over injected
  values; no settings-registry row moved, because every wrapper keeps its explicit
  `env::var(CONST)` read. `gen` became a reserved word (the two generated-code modules are
  `codegen` now), let-chains are stable (clippy's machine fix collapsed every collapsible `if` nest in the tree, 157 files), and the style-edition reformat
  is the bulk of the diff — with ONE of its hunks reverted after review: a `return` in `vike-app` that clippy, running under the `thin` feature, judged needless and that the default `fat` build depends on. `resolver` stays `"2"` on purpose — dependency resolution is a separate
  decision from the language edition.

### Fixed
- **The credential store's MODE, its SYMLINK and its DUPLICATE keys all survive a write now**
  — `vike_secrets::save_credentials`, so the GUI's Connections editor and cTrader's token rotation
  get these as well as the new CLI verb. The atomic temp+rename replaces the destination inode,
  which silently discarded two properties: a `chmod 600` store came back at the umask default
  (0644 — world-readable live signing keys, at the moment of a rotation that printed success), and
  a store kept as a SYMLINK into an encrypted or root-owned volume was replaced by a regular file
  full of plaintext inside the project directory, while the real file kept its pre-rotation
  contents for anything reading it by its own path. The mode is carried across now (a store this
  code CREATES starts at 0600 rather than inheriting the umask), and a symlinked store is resolved
  and written THROUGH. Separately, the upsert replaced only the FIRST line matching a key while
  `parse_dotenv` is LAST-wins — so rotating a key a store happened to hold twice (an ordinary
  hand-edit, or `secrets template >>`, the append typo of the documented `>`) reported "replaced",
  journalled `Applied`, and left every loader in the workspace reading the OLD credential; every
  matching line is replaced now. A failed write no longer leaves `.secrets.env.tmp-<pid>` beside
  the store holding the whole thing in plaintext. ⚠ The mode and symlink halves are Unix-shaped and
  are therefore covered by `#[cfg(unix)]` tests that the Windows dev box cannot run.
- **A shipped binary no longer carries the build box's paths, and a release refuses an asset that
  names the box.** rustc embeds every registry crate's source path — on the runners, a path under
  the runner account's home — in panic-location strings, and nothing remapped it; the same tokens
  `publish_mirror.sh` refuses in SOURCE went out in every binary. Each asset-building job of
  `release.yml` now takes `rust-ci-setup`'s `remap-paths`, which appends `--remap-path-prefix` for
  the checkout and `CARGO_HOME` to `RUSTFLAGS` (and `-ffile-prefix-map` to `CFLAGS` for the C in
  the tree), then runs `scripts/refuse_box_paths.sh` over every asset its manifest part names. The
  token set is ONE file, `scripts/forbidden_tokens.ere`, read by that guard and by
  `publish_mirror.sh`'s FORBID scan — which now also runs over the staged release assets before
  they reach the mirror. ⚠ The remap alone would have left the next tag RED on every asset lane,
  because the remap rewrites only what the compiler emits: measured on the real v0.1.18 assets,
  `vike-app-fat`/`-thin`/`.exe`, `backtest`, `vike-datahub` and the `vike` multicall each carried
  the runner's checkout path through `env!("CARGO_MANIFEST_DIR")` — the dev-checkout rung of the
  hist-store ladder, a string literal no flag touches — and everything linking `vike-ml` carried
  a box name in two error strings. Both producers are fixed: the three shipped ladder sites
  (`vike-app`'s `studio_store_root`, `vike-backtest`'s `binutil::repo_default`, `vike-datahub`'s
  `run`) pass that rung under `cfg(debug_assertions)` only, so a `--release` build resolves from
  the project walk down and carries no checkout path at all (`vike_model::store_path`'s
  `resolve_store_root` takes `Option<&Path>` for it, and its module doc argues the trade), and
  `vike-ml`'s error text names "the training box" rather than the box. `build_lightgbm.sh` also
  stops recording `hostname` in `lightgbm.PROVENANCE`; the copy cached on the release runner still
  carries one, but that hostname is not in the token set, so nothing refuses it and no rebuild is
  forced — this entry used to say the guards would, which was wrong.
- **The public mirror's release withholds what embeds somebody else's proprietary code, and the
  binstall URL cannot be stolen by the dataset release.** Two findings against the mirror step
  above, both from reading the REAL v0.1.18 manifest: the derived set would have re-published
  `vike-tradehub-fxcm` (the release runner DOES stage the ForexConnect SDK — the "no CI runner has
  it" assumption was stale) and the JForex shadow jar, which bundles Dukascopy's client libraries.
  `scripts/nonfree_release_assets` is the one spelling of what stays private; the asset step subtracts
  its rows from the manifest before it downloads, verifies or uploads anything, and the mirror's
  `SHA256SUMS` is the private one with those rows removed. And `releases/latest/` — the URL the
  template renders — is a property any release on the mirror could take: `publish_starter_data.sh`
  recreates the `starter-data` release at HEAD on every refresh, so a refresh after a snapshot made
  THAT the latest release and 404'd the install line. It now passes `--latest=false`, the version
  release passes `--latest`, and `packaging_gate.rs` holds both scripts and the withheld set.
  `docs/decisions/0037-…` records the withholding verdict and what would lift it.
- **The toolchain gate was blind to the copy every CI job actually uses.**
  `the_toolchain_version_has_exactly_one_value` scanned `.github/workflows/` and nothing else,
  while every Rust job installs its compiler through `./.github/actions/rust-ci-setup`, whose
  `toolchain:` line is a version literal like any other. `mutants.yml` carried a comment asserting
  that input was "counted once" by the gate; it was not. An unscanned copy contributes no entry,
  so a stale value there left the one-value assertion green while the shared setup step installed
  one compiler and `rust-toolchain.toml` overrode it to another. The scan now walks
  `.github/actions/**/action.yml`, and `the_shared_setup_action_is_scanned` fails if it ever stops
  reaching it — the one-value pin cannot catch that class on its own.

## [0.1.18] - 2026-09-04

### Fixed
- **`docker pull vikeio/vike-tradehub:latest` works.** It never had: `release_container_image.sh`
  pushed `:$version` and nothing else, so the floating tag was never created on either registry —
  while the README's FIRST command is that pull. The failure was invisible from the inside, because
  `pull vikeio/vike-tradehub:0.1.17` succeeds and the registry looks healthy to whoever published
  it; only a stranger following the README hits the 404. `push_latest` now moves the tag after each
  push, and REFUSES to move it for a pre-release — `release.yml` fires on `tags: ['v*']`, which
  matches `v0.2.0-rc1` as readily as `v0.2.0`, and `latest` means "the newest thing a stranger
  should run".
- **The published image's OCI labels pointed at a repository nobody can open.**
  `org.opencontainers.image.source` and `.documentation` both named the PRIVATE development
  repository, so both 404'd for everyone who pulled the public image. `source` is now the public
  mirror; `documentation` is the website rather than a repo path, because `docs/` is excluded from
  the mirror and a mirror blob URL would 404 just as reliably.
- **The desktop app showed an internal repository name to its own users.** The window title and the
  About menu read `vike_trader_rust`; they now read **Vike Trader**, the product name.

### Changed
- **The public mirror is `github.com/vike-io/vike-trader`** (was `.../vike`), and this repository is
  `vike-io/vike-trader-private` (was `vike_trader_rust`). Bare "vike" search results belong to
  vike.dev, the Vite meta-framework, so the brand is "Vike Trader", two words. `STARTER_BASE` —
  compiled into every shipped binary for the starter-dataset download — was REWRITTEN rather than
  left riding GitHub's rename redirect: a redirect is a trap rather than a safety net, working right
  up until somebody claims the freed name, and "vike" is a common word. `starter_dataset_gate` now
  detects both private-repo spellings for the same reason.

## [0.1.17] - 2026-09-03

### Added
- **The source is public** — `github.com/vike-io/vike-trader`, published by `scripts/publish_mirror.sh` and
  by nothing else. This repository stays private, and not as a preference: every workflow here runs
  on self-hosted runners and six fire on `pull_request`, for which GitHub runs the workflow file
  FROM THE FORK — so a public repository would mean anyone can execute code on the box that signs
  orders. The script selects by ALLOWLIST (a directory added next month does not ship until someone
  adds it), redacts the box names, paths, usernames and private IPs that ~300 files mention in
  passing, then scans the RESULT and **refuses to publish** if a forbidden token survived. It is a
  SNAPSHOT, one commit per release, because a mirror with history hands back every withheld file
  from an earlier commit. `docs/`, every `CLAUDE.md`, `.github/`, `justfile`, `scripts/` and the
  live `settings/*.toml` are never published, and `publish_mirror_gate.rs` holds that by RUNNING
  the script and inspecting the tree it produced.
- **A fresh install has data.** Three routes, because the reason a new box is empty differs: a
  deterministic **synthetic demo tape** (`vike-data`'s `demo` module, written under venue `demo` so
  it can never be mistaken for a real row) via `backtest --seed-demo`; a **keyless venue fetch**
  (`--fetch`, public klines, no credentials); and a **published starter dataset** (`--fetch-starter`,
  SHA256-verified) for a box that cannot reach a venue at all. Plus `--export` to Parquet.
- **The venue capability tables can now leave the repo.** `vike-ops`'s `docs_data` bin renders
  `venues.json` and `stats.json` as release assets, derived from the CI-gated tables themselves
  (`VENUES` through `caps_for`, `venue_margin_support`, `fee_schedule_for`, `amend_semantics`,
  attribution and `venue_tif`) — so a docs page or a venue matrix reads what the gates enforce
  instead of hand-copying it. Every hand copy of a roster in this tree has rotted.

### Fixed
- **The Connections picker never saved which backend it picked, and a bare launch signed with the
  wrong key.** Two defects on one promise — *configure a node once, launch bare afterwards* —
  found by installing the released Windows thin client and driving it against three live
  `vike-tradehub` daemons. Nothing ever SET `backends.json`'s `active` pointer, so a node added and
  connected in the GUI stayed `"active": null` and the next launch silently observed a DIFFERENT
  daemon with no error; and the startup connect discarded the record and kept only its address,
  signing with the fixed `VIKE_TRADEHUB_OBSERVE_KEY` instead of the record's own key name —
  `tradehub observe auth denied: bad mac`, on a retry loop, forever. `backend_conn::active_after`
  is now the one writer of the pointer, and `startup_backend` resolves a whole RECORD;
  `--observe ADDR` is unchanged.
- **The viewer no longer refuses to start.** A thin build used to `exit(2)` without
  `--observe <host>:<port>`, so a mistyped flag closed the window instead of opening one. The
  address now falls back to the registry's active backend and then to a local default, and the
  status bar says whether it connected — which is the thing a person can act on.
- **The containerised thin client showed a blank page and exposed its VNC port.** Whoever reached
  `6080` drove the same GUI, `x11vnc` listened on all interfaces without a password, and the noVNC
  page needed a manual click before it rendered anything. The port is now loopback-bound with an
  opt-in password, and the page auto-connects.
- **Linux GUI windows drew tofu boxes.** Every font path in `install_fonts` named a Windows
  location, so the window-control glyphs (`✕ ─ □`) resolved to nothing off Windows. Font
  candidates moved to `vike-app-core` with a test that every named family is BOUND on the platform
  the test runs on — the gate whose absence shipped this.

### Removed
- Six inert planning documents, and `docs/superpowers/`'s index is now gated as 1:1 with the tree
  rather than claiming to be.

## [0.1.16] - 2026-09-02

### Added
- **A licence.** The tree carried none at all — `licenseInfo: null`, with signed trading binaries
  already shipping to the end users `docs/decisions/0014` names. It is now **FSL-1.1-ALv2**
  (`LICENSE.md`, `docs/decisions/0034`): source-available today, each version irrevocably becoming
  Apache-2.0 on the second anniversary of its own release, with paid support and consulting
  explicitly permitted — the reason it was chosen over Commons Clause, which bans exactly that.
  Declared as `license-file` in the workspace manifest, never `license`: FSL has no SPDX id, and
  `deny.toml`'s licence gate stays green only through the `private` exemption, which its comment now
  explains instead of claiming the crates are proprietary.
- **`vike` — one executable carrying every headless tool** (`crates/vike`): `vike-cli`,
  `vike-tradehub`, `backtest`, `vike-datahub`, `vike-recorder`, `vike-study`, `tearsheet`,
  dispatched by argv[0] (an installed symlink) or `vike <tool>`. MEASURED on the CI box: the three
  DataFusion-linked binaries were 128 + 106 + 107 MB, each with its own static copy of the same
  closure; `vike --features full` is **128 MB** — the size of the largest tool it replaces. Every
  existing binary is kept as a thin shim, so every `ExecStart=`, `CARGO_BIN_EXE_*` and installed
  path still resolves. One tool per process is ENFORCED (a second dispatch panics), the dispatcher
  itself boots nothing — `multicall_gate` holds both, because `one_owner` structurally cannot see a
  dispatcher-plus-tool double boot. Thirteen settings-registry rows moved `Binary` → `Injected` on
  the way, which is that registry's declared target state, reached by threading parameters rather
  than by exempting ratchets.
- **The container image is the whole product, published where people can get it.**
  `docs/decisions/0035` lifts `0026`'s additive boundary: the image now carries every headless tool
  (as the one `vike` binary plus seven symlinks — 128 MB against ~500), the ForexConnect runtime so
  FXCM trades from a container (the author's distribution grant is recorded in `0035`; the `.so`
  set stays out of the release assets, where its 25 MB would tax every deploy), a portable Temurin
  JRE staged as a tool so Dukascopy's sidecar runs — no `apt-get`, the no-packages gate still holds
  — and it is pushed to **GHCR** (`ghcr.io/vike-io`, the job's own token, one `packages: write`
  scope, no stored secret) and **Docker Hub** (`vikeio/…`, the shopfront, a stored PAT taken
  deliberately). The privileged `vike-image-build` gained a `push` verb whose two destinations are
  CONSTANTS in the script — restoring push via `env_keep` would have handed the caller the
  destination — with the token on stdin and `docker logout` in a trap.
- **`vike-cli secrets template`** — the whole credential grid (every roster venue × SIM/DEMO/LIVE ×
  the credential suffixes, plus attribution keys), with EMPTY values, derived from `VENUES` so a
  new venue appears the day it joins. Prints to **stdout only**: a `--out` flag was deliberately not
  added, because the moment the command owns a path it can truncate a live store. The legacy
  `MAINNET` tier is read by the loader but not emitted — a first store should not be taught a
  spelling being retired. Composition lives in `vike_model::credential_keys::starter_keys`, because
  the registry gate reads a `credential_key` call site as "this crate READS these variables", and a
  command printing names reads none.
- **The the CI box daemon runs what users run**: the shipped `vike-tradehub` asset is now built with
  telegram, polymarket, record-feeds and materialize — decided so a production box can simulate its
  users. `fxcm` stays out of it (a hard `DT_NEEDED` dies at exec where the SDK is absent) and
  remains the separate `vike-tradehub-fxcm` asset; the two release gates that had forbidden ANY
  feature on the deployed pair were narrowed to their actual reason, native linkage.
- **The study runner that can FIT**, and panel provenance reproducing the engine bit-for-bit
  (#1584, prior session's work landing in this span).

### Fixed
- **Three of eight image binaries never built** (#1595): the roster named `vike-run` — a LIBRARY
  whose package builds no bin of its own — and `vike_recorder`, the file name where the manifest
  renames the target. Every text gate was green through it, because all three files AGREED on names
  that do not exist. `every_rostered_binary_is_a_real_cargo_bin_target` now asks `cargo metadata`
  rather than the filesystem.
- **`release-image.yml` could never fire** and the thin image carried seven drivers it cannot load
  (#1582, prior session); the image workflow's download list was also a hand-kept three while the
  image grew to eight — both the list and the expected checksum count now derive from the staging
  script's roster.
- **A venue outage no longer pages as our incident** (#1583, prior session).
- **`key_permissions.rs`'s module doc claimed the withdraw gate was not wired** while
  `vike_mount::make_engine` has called it for every credentialed binance MAINNET mount; corrected,
  with the survey of all fourteen venues recorded where the deferred per-venue table will need it —
  only bybit/okx/deribit can be probed at all, and for polymarket/hyperliquid/aster the capability
  belongs to the KEY ITSELF, where an all-`None` UNKNOWN that never refuses is the worst answer.

### Changed
- `docs/decisions/0026`'s "no registry credential exists on that box" corrected: a `github.token`
  is present in every workflow run, bounded by declared permissions — the distinction that made
  GHCR publishing one scope instead of a stored secret.
- Dependency bumps: the cargo minor-patch and major groups (#1580, #1581).

## [0.1.15] - 2026-08-31

### Added
- **The backend ships as a container image, and so does the client.** Two images, one per half of
  the split-plane architecture: `vike-tradehub` (the daemon) and `vike-thin` (the observe-only GUI).
  Both are built by `.github/workflows/release-image.yml` from the assets the Release already
  published, verified against its own `SHA256SUMS` before anything is copied in — so "built from the
  same bytes the release ships" is checked rather than inferred from sitting in the same directory.
  `docs/decisions/0026` is amended twice for this: the `"only at a real need"` deferral is lifted for
  `vike-tradehub`, and the GUI exclusion is narrowed to what it actually argued — GPU **passthrough**,
  not the application, which this repository's own `png-export` lane has been rendering on Mesa
  lavapipe with no GPU on every PR. The native path is untouched and remains the reference
  deployment; the image is additive or it does not happen.
- **Windows GUI assets — `vike-app.exe` and `vike-app-thin.exe`**, cross-built on the Linux release
  runner. The release published a Linux GUI only, so the largest desktop population had no native
  client and was pushed onto a container costing 1–3 CPU cores where an `.exe` costs about a tenth
  of one. That gap was never a decision; nobody had checked whether the cross-build worked. It does
  — and the binary was RUN on a Windows box, where it enumerated a real GPU through Vulkan,
  connected to a containerised daemon and rendered the full UI. ⚠ `x86_64-pc-windows-gnu`, not
  `-msvc`: the msvc target needs the MSVC linker and Windows SDK, which no Linux runner has. Worth
  knowing, because day-to-day development on a Windows box builds msvc.
- **`vike-app-thin`**, the Linux observe-only GUI asset — 51 MB against the fat build's 152 MB, and
  the input the client image is built from. `0.1.14` anticipated exactly this twin.
- **`vike-tradehub/ibkr`.** `vike-run` had carried the feature since the bridge landed and
  `vike-app` forwarded it, but this crate did not — so the GUI could mount IBKR live while the
  headless daemon fell through `make_engine`'s `("ibkr", _)` to paper **in silence**. The `ibkr` CI
  lane and its trigger set widen with it, so the arm is now compiled by something.
- **`VIKE_FRAME_LOG`** — an opt-in per-second frame-rate log. A measurement hook, not a feature.

### Fixed
- ⚠ **`vike-app` could not start on Linux at all.** `install_fonts` loaded every face from a
  `C:\Windows\Fonts\…` path, so off Windows the named families were never bound and epaint panicked
  on the title bar in frame one. **The release had been publishing a Linux GUI asset that had never
  been run** — `png-export` renders `vike-chart`/`vike-studio` EXAMPLES rather than this binary, and
  `app-check` only compiles it. Found by running it in a container. The regression test is
  platform-blind and mutation-tested both ways: it lays text out rather than checking membership,
  because a family bound to an EMPTY list passes a membership check.
- **An absent observe key signed with `""` and reported it as `bad mac`.** `unwrap_or_default()` on a
  missing key produces a well-formed HMAC the node rejects, so the operator saw "wrong key" when the
  truth was "no key". The daemon already got this right from its side; this is the client's half of
  the same sentence, and it names where the key is actually read from — the credential store, which
  an environment variable alone does not reach.
- **A warning that contradicted the chart in front of the operator.** `ensure_feed` ended "this chart
  will stay empty until one is", which is false in `--observe`: that build links no venue bridge, so
  every chart reaches that arm while the chart fills from `WireSnapshot::bars` over the node
  protocol. Measured while it logged on repeat against five hours of live candles.
- **The release-image workflow used `gh`, which is not installed on the release runner.** It would
  have failed on its first real run. `release.yml` already carried that rule beside its own publish
  step; the new workflow had not inherited it.

### Changed
- **`<project>/data` is now `<project>/market_data`.** It sat beside `<project>/user_data` and the
  pair read as one concept split in half — the question "which data?" had no answer in either name.
  Two doc claims went with it, both false beforehand: `hist` was never "the only thing in it"
  (`ticks` and a checkout's `bench_hist` are siblings), and the folder is wider than "market" — the
  hist store also holds this account's own `equity`, `exec_fill` and `exec_order`.

## [0.1.14] - 2026-08-29

### Added
- **The GUI ships as a release asset.** `vike-app-fat` — the full desktop app (the crate's default
  `fat` features: the local trading core, every venue bridge, the Studio's local store) for Linux
  x86-64 — is now built by `.github/workflows/release.yml` from the tagged commit, with its SBOM
  embedded by `cargo auditable` and its line in that release's `SHA256SUMS`, exactly like the two
  server binaries beside it. Obtaining the GUI used to mean compiling the workspace. The asset name
  carries the configuration on purpose: nothing is published as a bare `vike-app`, so a `thin`
  (`--observe`-only) twin could join later without re-pointing a single instruction that ever quoted
  the fat one. The tag path also grew the gate it was missing — `vike-app` is excluded from the
  derived crate roster a release re-validates, and `ci.yml`'s `app-check` runs on no tag, so that
  crate's unit tests now run on the release path instead of the artifact being published unrun.

## [0.1.13] - 2026-08-25

### Fixed
- **A reconcile pass that changed nothing logged anyway, once per pass.** `reconcile_reports` emitted
  `reconcile pass folded venue=… events=0 alerts=1` on every pass carrying alerts — ~1,440 lines a
  day per venue, and wholly redundant when nothing moved. Measured on the live box an hour after the
  0.1.11 deploy: 11:41:43 and 11:42:43, one minute apart, byte-identical. The line now reuses the
  held-divergence announcer's own "did anything happen this pass" answer rather than growing a
  second rate limiter beside the one 0.1.11 shipped, so the pass line and the backlog summary fire
  together by construction instead of because two cadences happen to agree. A pass that folds events
  still logs every time, and a venue with nothing held stays silent exactly as before — no heartbeat
  was invented where there had never been a line.
- **Deribit's post-reconnect replay stopped forever on the first socket close, in silence.** The A3
  resync transport connected once and nothing re-dialled it, so a venue-side close ended the replay
  of order/trade history across reconnect gaps for the life of the process — with every error
  swallowed, so nothing surfaced. It is the last of three never-heals sockets in that bridge; the
  other two were fixed in 0.1.11. Which cure applied was traced rather than assumed: that transport
  can carry exactly three frames ever — the auth, the order history read and the user-trades read —
  all non-matching-engine, so a re-dial plus a straight retry cannot double anything, and the licence
  is now stated on the type as what it MAY send. The heal deliberately did NOT go into the shared
  history helper, which the fill sentinel also calls on the EXEC ORDER socket — the one that carries
  `private/buy`, whose re-dial policy is decided elsewhere and must not gain a second, unreviewed
  entry point reachable from a timer. An outage is now bracketed by one warning naming the
  consequence and one recovery line, and the latch re-arms, because a latch that fires only once
  makes a broken lane look healthier the longer it stays broken.

## [0.1.12] - 2026-08-25

### Added
- **A venue can mount SEVERAL accounts.** `make_engine_accounts` returns one engine per ACTIVE
  account — its per-account ceiling permits it, its credentials load, the venue's arm can address an
  account at all, and the symbol-collision rule does not refuse it. Selection and the Data Manager's
  Effective column are ONE function, so the screen cannot disagree with the mount. A default-account
  box is identical, measured: one engine per roster venue, `route_key == venue`, empty
  `live_venues`, and a ledger record with no `account` key at all. ⚠ Only binance/bybit/okx/deribit
  consume the account-aware credential loader; a labelled account is REFUSED elsewhere rather than
  half-supported, because arming one would otherwise have built a live client on the first account's
  credentials.

### Fixed
- **The journal's p99.9 tail is gone rather than reduced** — 37 150 → 3 342 ns (11x), interleaved
  medians of 8 reps, bands that do not touch; the journal-less baseline is 3–7 µs, which is where it
  now sits. The warm window was ~20 ms of runway refreshed only AFTER a 36–141 ms blocking `msync`,
  and it was aimed at the watermark — behind the appender. ⚠ Two earlier claims recorded in this
  tree were wrong and are corrected at the code: the tail IS first-touch faults (the null result came
  from a harness appending 85 B records while the gate appends 275 B), and serialization is 61 % of
  the append's median but only 3 % of its p99.9 — the reused-buffer change was measured and NOT
  shipped. Production's 64 MiB configuration was already at 6.4 µs; this removes a large gate number
  and a smaller real one.
- **A HELD reconcile divergence names WHAT diverged**, and its dedup IDENTITY stops riding that text.
  `kind=PositionOnlyExternal detail=PositionOnlyExternal` was what the live box printed — a line
  raised to answer *which* answering *the same*. `Divergence::describe` is a total match, so a new
  variant is a compile error rather than one that inherits an empty description. ⚠ Because
  `HeldId::new` builds an un-keyed alert's identity from its `detail`, a detail carrying `qty=` would
  have made a drifting position a NEW alert every pass — the ~1,560-a-day bug `recon_held_tests.rs`
  exists to prevent, invisible to its own fixture, which holds qty constant. `identity_detail` is the
  churn-free half.

## [0.1.11] - 2026-08-25

### Fixed
- **The IG trade stream sent a made-up Lightstreamer client id, so the fill lane never worked — for
  any account, demo or live.** `LS_cid` is Lightstreamer's client-type LICENSING field; the trade
  lane sent `"vike-trader-rust"` and IG answered `CONERR,71,License not valid for this Client type`
  on every dial since the day it was written. The market-data lane always sent the TLCP-2.1.0
  mandated value from its own `LS_CID`, which is why feeds worked while fills did not — and why the
  split read as an account-permissions problem for weeks. Proven on the wire against the demo
  gateway with one login and two client ids, everything else held constant: the old value returns
  `CONERR,71` and closes; the mandated one returns `CONOK` and streams. The trade lane also carried
  its own copies of `LS_PROTOCOL`, `form` and `urlencode` — that duplication is what let the two
  spellings of one handshake drift — and `lightstreamer.rs`'s `form` doc already CLAIMED the sharing
  that did not exist. All four now come from one place. This settles the create_session handshake,
  not the fill lane end to end.
- **A permanently refused IG stream wrote an error line every second, forever.** Measured on the CI box:
  53,160 lines and 23 MB in one day from one loop, dead flat for 17 hours, 99.5% of the box's entire
  error volume — for a condition whose own message said it was permanent. The dial-failure path had
  the same defect unbounded and was worse (an unreachable endpoint warned ~78,500 times a day); it
  had been argued safe because "each drop costs a live session", which is true of exactly one of the
  six sites that produce it — the other five never establish a session at all. Retry and log policy
  now live in a pure state machine that takes the clock as an argument, so the log RATE is
  unit-tested rather than being a property of a running daemon. Both lanes now cost ~26 lines a day,
  and the driver reports the CONERR code as evidence instead of asserting a cause it cannot
  distinguish.
- **A Deribit socket closing mid-submit reported an order REJECTED that the venue may have
  accepted.** Only a response timeout was treated as ambiguous, so a mid-request close — the common
  first error on a dying socket — took the definite path and synthesized a terminal reject for a
  possibly-live order, stranding a phantom position. Every failure out of the read half is
  post-send by construction. The rule already existed and this transport was the one not following
  it: `vike_bridge_core::transport::read_body_ambiguous` states that a read failure is always
  ambiguous, never `code: 0`. Resolution is re-dial then RE-QUERY, never re-send — a re-dial
  rewrites no order frame. Verified against testnet that the label query returns terminal orders,
  cancelled and filled alike, so resolving on its answer cannot reject a fill that happened.
- **A Deribit reconcile lane died on the first socket close and never recovered.** The recon
  order-WS connected once at mount and nothing re-dialled it, so one venue-side close disabled
  reconciliation for the life of the process — 795 identical failures in a day, one problem that
  could not heal, while the daemon reported healthy. Every other socket in that crate already had a
  reconnect lifecycle; this one was given a dedicated transport and inherited none of it.
- **A held reconcile divergence re-announced itself every interval, and its alert store grew without
  bound.** 0.1.10 made the divergence visible and reasoned that the refresh path staying silent
  would bound it. It did not: an alert carrying no dedup key never takes the refresh path — it
  APPENDS — so every pass was a fresh raise. Measured on the CI box under `quarantine`: 396 warnings in a
  day for one bybit `PositionOnlyExternal`, with a genuinely new divergence indistinguishable among
  them, and ~1,560 store rows a day, each held in memory and projected into every published
  snapshot. A held divergence is a STATE, not an event: it is announced on entering and on clearing,
  with a slow backlog summary, and the store keeps one row per distinct identity. Confirming one now
  resolves what previously needed one confirm per pass.

### Security
- **A Rhai strategy — and a user indicator — could read any file on disk.** (#1520)

### Added
- **The venue sends a funding premium and we were discarding it** — recorded as `kind=perp_metrics`.
  (#1521)
- **A study can be RUN from the Studio GUI**, either tier, with one results surface. (#1523)


## [0.1.10] - 2026-08-24

### Fixed
- **The `ch_http` fake client survives `ETXTBSY`.** The v0.1.10 release re-validation refused to
  publish on `spawn: Text file busy (os error 26)` from a crate the tagged change never touched —
  which is exactly what that gate is for: a tag runs the FULL roster, a merge runs only the affected
  set. The test writes an executable and immediately spawns it; `std::fs::write` closes its handle,
  but a binary's tests run in parallel threads of one process and a `fork` in any other thread hands
  the child an inherited copy of the write descriptor. Retried in
  the TEST, matched on the errno alone, and BOTH call sites are wrapped: the `select` twin has the
  identical race and had simply not lost the coin toss yet.
  ⚠ **SUPERSEDED 2026-09-14, and how it went stale is the point.** This entry read *"CI cannot see
  it — the fast lane runs nextest, one process per test — so only the tag's plain `cargo test` pass
  reaches it"*. ⚠ A first correction called that FALSE. **It was not false — it was CRATE-SCOPED
  truth written as a CI-SCOPED claim**, and that distinction is the whole lesson, so the stronger
  wording is withdrawn here rather than left standing.
  Measured at the `v0.1.10` tag itself: `scripts/ci_feature_suite.sh`'s `light-consumers` arm
  ALREADY ran a plain `cargo test -p vike-cli` — one process, parallel threads, the exact shape this
  race needs — but `despite_text_file_busy` lived in `crates/vike-research/tests/ch_http.rs`, and
  `vike-research` appeared in NO plain-`cargo test` arm, only the nextest roster. There was also no
  `crates/vike-cli/tests/data_cli.rs` yet. So for the crate this entry was about, "CI cannot see it"
  was exactly right.
  What broke it was not a mistake in the reasoning but the pattern MOVING: the same
  write-chmod-spawn shape was later planted in `vike-cli`, which that lane does run, so the race
  became reachable on any PR whose affected set names it — measured twice on 2026-09-14, on two
  unrelated PRs. ⚠ And the cure itself is GONE from the tree: `vike-research` dissolved in #1524,
  taking `ch_http.rs` and `despite_text_file_busy` with it, so `main` carries no `os error 26` in
  any `.rs` at all. The diagnosis outlived its implementation, leaving the knowledge as prose here
  and no executable answer anywhere. See the `[Unreleased]` entry that cures the whole `vike-cli`
  roster.
  **The durable rule: a property of one CRATE recorded as a property of CI is a claim whose basis
  can be removed without touching the claim.** Say which lane runs which crate, not what "CI" can
  see.
- **A HELD reconcile divergence now reaches the LOG, not only the in-memory ring.** Found on the
  live the CI box box an hour after the 0.1.9 deploy: `reconcile pass folded venue=bybit events=0
  alerts=2` every 60 seconds for hours, zero lines at WARN or above, and no way for anyone reading
  logs to learn WHICH two divergences were being held. `note()` writes a ring reachable only
  through the control channel, and that channel is off on the shipped daemon — so the COUNT reached
  the operator and the CONTENT did not. The raise path now emits one `warn!` carrying the
  divergence kind, venue and detail; the REFRESH path stays silent on purpose, because a divergence
  held for a day re-raises 1440 times and logging each is how an operator learns to filter out the
  whole channel, raise included. `crates/vike-ops/tests/held_divergence_is_visible_gate.rs` anchors
  on the ORDER rather than on presence — a `warn!` anywhere in that 4000-line file would satisfy a
  presence check while the raise path stayed silent.

## [0.1.9] - 2026-08-24

### Added
- **A ForexConnect-linked `vike-tradehub` is obtainable from a release.** A release built on a
  runner with the proprietary ForexConnect SDK staged now attaches an ADDITIONAL
  `vike-tradehub-fxcm` asset — the same tagged commit with `--features fxcm` — covered by the same
  `SHA256SUMS` and installed by nothing. The ordinary `vike-tradehub`/`vike-cli` assets are
  byte-identical: a feature build gains a hard `DT_NEEDED` on `libForexConnect.so` and dies at exec
  where those libraries are absent, so flipping the feature on the name the deploy installs would
  have broken the next deploy of the live daemon. `scripts/release_fxcm_artifact.sh` builds into a
  scratch target directory, reads the produced ELF's dynamic section (a `--features fxcm` build
  without an SDK compiles a STUB and exits 0, so the exit code proves nothing), re-checksums the
  default artifacts before emitting anything, and declines silently where no SDK is staged.
  `docs/ops/fxcm-forexconnect.md` Step 5 carries what an operator must then do — the
  libraries are NOT in the release and `just fxcm-package <root>` is a required step, not an
  assumption.

### Fixed
- **The ForexConnect rpath reached `vike-fxcm`'s own test binaries and nothing downstream.** Found
  by building the artifact above, which is the first time anything downstream of that crate had
  ever been linked against the SDK: the binary came out with `NEEDED libForexConnect.so` and no
  rpath tag at all. `crates/bridges/fxcm/build.rs` was emitting all three link args correctly —
  cargo scopes `cargo:rustc-link-arg` to the targets of the package that emitted it, while
  `rustc-link-lib`/`rustc-link-search` propagate, which is why the `-lForexConnect` arrived alone.
  #1470's fix was therefore real and its reach was one package. The release build now sets the same
  args on the final link, pinned equal to `build.rs`'s own emissions by a gate and checked against
  the produced ELF before anything is published.

## [0.1.8] - 2026-08-22

### Added
- **The two runtime tools are OBTAINABLE.** A release now attaches the Dukascopy JForex sidecar jar
  and the pinned LightGBM binary (with the `PROVENANCE` file that `LightGbmCli::new` refuses to run
  without) alongside the two binaries, covered by the same `SHA256SUMS`. `just fetch-tools`
  downloads and verifies them into `<project>/bin/<tool>/`. Before this, `research run cohort`
  hard-failed on a fresh install because nothing shipped LightGBM, and Dukascopy only worked
  because a 43 MB jar was force-added past `.gitignore` into every clone's history.

### Fixed
- **The startup preflight's clock budget was sized against six reads, and the roster outgrew it.**
  The budget was a flat constant, sized against the measurement its own module doc
  records — six real reads, 1781 ms, the CI box 2026-08-09 — and never revisited as venues were added.
  By 2026-08-22 the wired roster's pinned healthy readings summed to 2973 ms, and one unreachable
  venue costs a full 3 s `CLOCK_READ_TIMEOUT`: 5973 ms needed against 5000 available. Observed on
  the dev box, where bybit's endpoint stopped answering and EIGHT venues reported their clock "not
  read" — `aster` among them, which runs against mainnet in practice and whose auth binds the clock
  into the order path. The budget is now DERIVED from how many venues are actually read
  (`clock_budget_for`), clamped between the old value as a floor and a ceiling, so a venue joining
  the roster buys the leg more time instead of squeezing the venues behind it. The sizing
  requirement — absorb one dead venue and still read the rest — is arithmetic over those same
  pinned measurements, so the roster outgrowing the allowance reddens a test instead of silently
  starving the tail. A per-venue deadline was considered and rejected;
  `docs/decisions/0027-clock-budget-derived-from-the-roster.md` records why.

### Changed
- **The JForex jar is no longer committed**, and `.gitignore` has no re-include chain left — the
  repository's one force-added path is gone. `crates/bridges/dukascopy/scripts/provision-jforex.sh`
  (and its `.ps1` sibling) fetch the jar instead of failing with "it is committed — bad checkout?".
  ⚠ **An offline clone therefore no longer gets a jar**; `JFOREX_BRIDGE_JAR` still names one
  outright and `--rebuild` still builds one from source. `README.md` carries both.
- **The jar's drift gate became a REPRODUCIBILITY gate.** With no committed copy there is nothing
  to drift from, but "the published bytes are re-derivable from the tag" is newly load-bearing, so
  `.github/workflows/jforex-bridge.yml` builds the jar twice from a cleaned build directory and
  requires the two byte-identical — the claim `build.gradle.kts` has always made and nothing ever
  checked. `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` gained the release workflow as a fourth
  JDK pin site, plus a test that the release still builds the artifact the gate proves.
- `.github/workflows/deploy.yml` follows the manifest instead of naming assets, so it downloads
  every file `SHA256SUMS` lists rather than only the two it installs — `sha256sum -c` verifies the
  whole manifest, and the same command runs again inside the root-owned deploy helper.

## [0.1.7] - 2026-08-22

### Added
- **A live smoke for the startup preflight's two legs.** It runs the real preflight twice — once
  with an empty credential map (clock reads only, the honest baseline) and once with the real store
  — and fails if any venue lost its clock check *only* when the credential leg ran beside it. The
  two-run control is the point: the budget legitimately starves venues behind a slow clock read, and
  only a baseline separates that from the defect below.
- **`just app-paper`** — runs the GUI with no credentials in scope, so every venue stays paper by
  construction. `vike-app` opens the live gate on the mere presence of credentials and a live mount
  refuses to start without an operator risk budget, so on a populated box the obvious "look at a
  chart" run could not start at all. That refusal is unchanged; this is the other way out, made one
  command. An optional path argument captures a framebuffer PNG.
- **A data-plane-only credential seam in the trading daemon** — a credentialed feed without an armed
  exec, so a venue's market data can be live while its order path is not.
- **One placement rule for third-party artifacts, plus a ratchet that keeps it.**
- **The GUI contact sheet gets a judge**, and the GUI's CI-excluded shell gets a coverage ratchet.

### Fixed
- **The startup preflight's clock budget was being spent by the credential leg.** `run_preflight`
  interleaves the two legs per venue, and a credential probe is a blocking authed read on that
  venue's own 30 s agent — so ONE unreachable venue spent the whole 5 s clock budget and every venue
  behind it reported its clock "not read", blaming "an earlier venue's clock read" and sending an
  operator to inspect rows that were all fast and healthy. Measured: a geo-blocked probe sat ~20 s on
  a TCP connect and cost alpaca, aster and hyperliquid their clock checks while the clock leg itself
  had spent 3.2 s of its 5 s. `aster` mattered most — it runs against mainnet in practice and its
  auth binds the clock into the order path. Each credential probe is now measured and the deadline
  pushed out by its cost, so the budget bounds the clock leg and only the clock leg.
- **OKX options were never in the catalog.** The listing was fetched as a bare `instType=OPTION`,
  which the venue refuses with `400`/`50015`; the module already documented that failure and made
  the loop tolerant so spot/perp/futures survived, but never fixed the call — so `asset_classes()`
  advertised `AssetClass::Option` while the picker received zero options and a warning fired on
  every start. Now fetched once per underlying, paired as `uly` (not `instFamily`, which serves only
  two of the four families). Measured: okx 2004 → 5162 instruments.
- **Alpaca reported a transport failure as `HTTP 401`** — an unreachable token mint read as "bad
  keys", answering a network condition with a credential rotation. `AuthError` now separates a mint
  that answered and refused from one that was never reached.
- **Two warnings that fired forever on a healthy box.** The network row WARNed with a developer's
  TODO on every credential-free start; it is now `NotApplicable` with its reason when no venue would
  mount live, and still WARNs whenever a venue is actually being checked. The Windows Vulkan loader's
  two per-start warnings about absent validation-layer manifests are pinned below WARN on the console
  via a new `NOISY_TARGETS` family in `vike-log`, which obeys the same rules as the credential pins
  (an explicit mention wins; a pin may only ever narrow).
- **The catalog defaults REFUSE** — "cannot enumerate" stopped answering as "empty store".
- **A pinned research price window slid an hour instead of extending.**
- **`App::new` stopped demanding an unconstructible type**, and a stale `Cargo.lock` stops reaching CI.
- **An empty `[Unreleased]` now REFUSES the cut** — v0.1.5 and v0.1.6 both shipped on a note nobody
  read, because an advisory line in a scrolling release log is invisible.

### Changed
- **CI economics**: co-firing small feature lanes share one matrix leg, and the `~/.cargo` GitHub-cache
  round-trip is gone from setup — it restored what was already on disk.
- **47 rotted re-export spellings** retired across nine audited crates.

## [0.1.6] - 2026-08-19

Written after the fact (2026-08-22) from the merge history: this release and 0.1.5 were both cut
while an empty `[Unreleased]` was a legal no-op, so neither wrote a section at the time. It is a
refusal now — `just release` will not cut a version that says nothing about itself.

### Added
- **More venues join the trading daemon's live-wired set** — alpaca, ctrader, oanda, deribit and
  ig. Each mounts a credentialed market-data lane in `vike-tradehub` the way binance and friends
  already did; deribit, oanda and ig also gained their first LIVE market-data adapters (oanda's
  chunked-HTTP pricing stream, ig's Lightstreamer TLCP spoken raw over the shared transport stack —
  and ig serves no depth, which the adapter says outright rather than reporting an empty book).
- **The datahub authenticates.** It served history reads, backtest compute over client-supplied
  Rhai, and — in a backfill-serve build — writes into the store, to anything that could reach the
  socket. It now speaks the tradehub node handshake with scoped reads and writes; keys ABSENT
  serves exactly as before, so an existing single-user install is unchanged. The verdict:
  [`docs/decisions/0025-datahub-remote-posture.md`](docs/decisions/0025-datahub-remote-posture.md).
- **One backend address really is one address.** A daemon can advertise the datahub it fronts
  (a new `datahub_advertise_addr` key in `config.toml`), so a client that configured the tradehub
  reaches the history plane too instead of being configured twice. Unset changes nothing.
- **Settings hot-reload for the classified-safe subset.** The console and file log levels apply on
  a running daemon without a restart; every other key is answered `restart_required`, and
  `policy.toml` is structurally excluded — no future key can make a risk ceiling hot-appliable. The
  periodic summary also stopped over-reporting what is mounted.
- **Runtime mounts survive a restart.** A strategy mounted over the wire is remembered as daemon
  state and comes back after `systemctl restart` or a crash; only an explicit unmount forgets one.
  The journal is deliberately untouched — topology is session state, not order state.
- **A detached Windows daemon can be stopped cleanly**, and CI now compiles the daemon for Windows
  so that path stops resting on one developer's box.
- **Data-Manager coverage over the wire**: the Partial column follows the store in remote mode
  instead of being marked local-only.
- `vike-cli strategy-status` — ask a running node what it is running (human table, or the wire
  payload verbatim under `--json`). Asking used to mean compiling a throwaway probe.
- `just creds-audit` — find credential files by CONTENT across every box, so a stray copy of live
  keys is discoverable rather than remembered.
- Polymarket has a feeds-only build: the market-data plane compiles without the order signer.
- Under `external-quarantine`, a reconcile `MissingFill` that carries no `client_order_id` is HELD
  rather than folded — a foreign order's fill inside the lookback now needs an operator claim,
  while coid-linked fills keep folding.

### Fixed
Places where a capability gap was answering as data — all the same defect:
- A history store that cannot serve depth now REFUSES instead of returning an empty read, so "this
  store has no depth lane" stopped being indistinguishable from "there are no rows".
- Studio: a series scan that FAILED stopped rendering as an empty store, and `depth` being
  unreplayable stopped being a silent omission.
- oanda: the live tier REFUSES instead of vanishing, and a rejected exec stream stops looping in
  silence.
- fxcm: a login that FAILED reddens the smokes instead of being reported as a green test.
- The venue smokes honour `VIKE_SETTINGS_DIR` — they had been skipping in silence wherever the
  credential store was not directly above them.

Also:
- cTrader's OAuth token parser survives the venue sending either field spelling.
- Studio's ChatPane takes its provider keys instead of opening the credential store itself.

## [0.1.5] - 2026-08-19

### Added
- **Aster joins the daemon's live-wired venues** — capability only, and the MAINNET warning travels
  with it: no aster testnet credentials exist, so an armed aster lane reaches the real account.
- **Backends are managed from the GUI** — add, edit and delete backend records in place, with
  key-name hygiene on the credential names each record points at.
- **Settings are writable over the wire.** A `config`/`preferences`/`flags` key can be edited on a
  running daemon behind the control scope and a typed confirmation: the would-be file is validated
  with the same loader that will read it back, edited comment-preservingly (untouched lines stay
  untouched bytes) and landed atomically. `policy.toml` is special-cased — a risk ceiling is not a
  remote edit.
- A core-free direct-bar path: third-mode klines go venue-direct instead of through a trading core
  that has nothing to do with them.

### Fixed
- The Data-Manager grid follows the store — remote inventory arrives over the `HistStore` trait, so
  the grid describes the store you are connected to rather than a local handle.
- The `just cockpit` SOCKS tunnel host is a parameter defaulting to the CI box; it hardwired the parked
  Dublin route, which no longer answers.

### Changed
- No `pub use` shims on a code move: when a symbol changes homes every call site updates, and a
  re-export kept so old spellings compile is a second name that rots. Aster's leftovers are gone.
- [`docs/decisions/0025-datahub-remote-posture.md`](docs/decisions/0025-datahub-remote-posture.md)
  opened (proposed), with the bind guard both routes need.

## [0.1.4] - 2026-08-18

### Added
- Split-plane Phase-1 safety trio: the observe snapshot names its daemon (`WireNodeIdentity` —
  profile name, mounted strategy, paper-vs-live, build identity), a Windows console-ctrl graceful
  stop (Ctrl-C / window-close now run the same bounded teardown as the unix SIGTERM arm), and
  `LiveLock` — one live process per venue account, so two live traders can no longer silently
  rewrite each other's positions through reconcile.
- Runtime backend switching in the GUI: `BackendConn` makes the observe connection switchable (one
  active backend, total state clear on switch), a persisted backend registry owns the records, key
  resolution and the control arming gate, and both status surfaces name the connected daemon —
  LIVE/paper plus its identity.
- Rhai strategies mount LIVE: `[strategy] rhai = "<path>"` in a daemon profile compiles the script
  through the same engine builder the backtest arm uses and mounts it on the same paper/live cores,
  with the script's audit hash logged at mount. The verdict and its fence:
  [`docs/decisions/0024-rhai-strategies-live.md`](docs/decisions/0024-rhai-strategies-live.md).
- Studio as a thin client: Studio operates on the `HistStore` trait (remote-store capable) and its
  default build is DataFusion-free, so it can browse a served store with no local query engine.
- Backfill-on-demand on the datahub wire: `Request::Backfill` has the backend fetch a bounded kline
  range once, write it through to the store, and reply only after the served handle reads it back —
  capability-negotiated, so a client refuses locally against a server that does not advertise it.
- Strategy verbs on the tradehub node wire: the read-only `Request::StrategyStatus` and the
  control-scoped `WireCommand::UpdateParams` — inspect and retune a mounted strategy without
  restarting the daemon.

## [0.1.1] - 2026-08-18

### ⚠ BREAKING — settings unification (2026-08-05 … 2026-08-06)

**If you are upgrading an existing install, read [`docs/ops/upgrading.md`](docs/ops/upgrading.md)
first.** It is the full table: what moved, from where, to where, what today's binary does about the
leftover (silent / warns / refuses / migrates itself), and the exact command to fix each one —
including the `/opt/vike` in-place-upgrade case under an unchanged systemd unit.

The short version: every setting, credential and program-written file now lives in ONE
project-owned directory.

```text
<project>/settings/policy.toml  config.toml  preferences.toml  flags.toml
<project>/settings/secrets.env                 every API key, every venue
<project>/settings/state/…                     program-written files
```

- **Credentials moved** from `<repo>/.env` to `<project>/settings/secrets.env`. Absent credentials
  ARE the live gate, so a leftover `.env` used to mean every venue silently on paper with nothing in
  any log. An absent store with a `.env` beside it now **warns** on every credential read and in both
  `vike-cli secrets` subcommands, naming both paths — a finding, never a refusal, because `.env` is
  also a legitimate systemd `EnvironmentFile`. Nothing reads, prints or moves its contents.
  The interim `~/.vike/secrets.env` and `~/.vike/secrets.enc` slots existed for about a day and are
  gone; they cannot be warned about, because the home-directory resolution that found them was
  deleted with them.
- **Settings TOMLs moved** from `~/.vike/*.toml` to `<project>/settings/*.toml`. Silent — an absent
  file means the code default.
- **`<project>` is resolved at RUNTIME** by walking up for a project marker: a checkout's WORKSPACE
  ROOT (the outermost `Cargo.toml` declaring `[workspace]`), else a deployment's own `settings/`
  directory. `VIKE_SETTINGS_DIR` names it outright and skips the walk — set it on a deployment.
- **Refused at startup, never ignored** (`vike-app`, `vike-cli`, `vike-tradehub`):
  `VIKE_MAX_ORDER_NOTIONAL` and `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` (→ `policy.toml`'s
  `max_notional_per_order`; a ceiling any exported variable can raise is not a ceiling),
  `VIKE_SECRETS_PASSPHRASE` (nothing consumes it), a leftover `<project>/vike.toml`, and the removed
  keys `policy.toml`'s `max_total_exposure` / `[rate] max_utilization` and `preferences.toml`'s
  `rate_utilization`.
- **State files** (`workspace.json`, `alerts.json`, `layouts/`, `studio_workspace.json`, `pace.json`,
  the Telegram ledger, the cTrader token, the REPL history, the rolling trace log) moved under
  `<project>/settings/state/`. Four migrate themselves on first write; the rest are silent and cost
  a layout or a cache, never trading.

### Added
- Security/CI hardening (2026-07-08 audit): `cargo-deny` supply-chain gate (`deny.toml` +
  `deny.yml`), SHA-pinned GitHub Actions with least-privilege tokens + job timeouts, pinned
  toolchain (`rust-toolchain.toml` 1.96.0) + MSRV, workspace `unsafe_code = "forbid"` lint,
  `rustfmt` gate, `gitleaks` pre-commit hook, checksum-verified JForex/Temurin/Gradle
  provisioning, `.gitattributes` EOL policy, a `justfile` task runner, and a weekly
  Windows compile check.
- Dependabot (alerts + cooldowned version updates) and this release pipeline.

<!-- Add the next release's user-facing changes under [Unreleased]. `just release` cuts the
     section under a version heading via scripts/changelog_release.sh — no hand edit needed.

     That script REFUSES to cut an empty [Unreleased], and the refusal lands before the commit,
     the tag and both pushes. It used to print a note and carry on, which is how 0.1.5 and 0.1.6
     shipped with nothing here. A release that genuinely has nothing to say is still possible, but
     it has to be said out loud: ALLOW_EMPTY_CHANGELOG=1. -->
