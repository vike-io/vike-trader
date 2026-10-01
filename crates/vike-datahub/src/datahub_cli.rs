//! `vike-datahub` — the data service command line, as a LIBRARY function.
//!
//! This was the binary's body until the multicall merge. `main` became [`run`], taking the
//! environment, the working directory and argv as parameters. Both cfg arms travelled — the
//! serve-datafusion server and the feature-off stub — so a build that cannot serve still answers
//! `--help` identically, which is the property `short_circuit` exists to hold.
//!
//! The working directory is a parameter for the same reason the environment is, and the settings
//! registry cannot see it: a `current_dir()` inside a library is ambient state its caller cannot
//! override. This body read it TWICE before the move and now receives one answer.
//!
//! Everything below is the binary's own documentation, unchanged.
//! The real server (a `DataFusionHist`-backed [`serve`](crate::serve) loop) is compiled
//! ONLY under the `serve-datafusion` feature, which is what makes the concrete `DataFusionHist`
//! backend nameable here. A default build compiles the inert stub `main` below instead, so
//! `cargo build -p vike-datahub` always produces a runnable (if non-serving) binary that compiles
//! cleanly. (NB this crate has no edge to vike-backtest at all — ruling 7 removed it — so nothing
//! about that crate's feature shape reaches here; `serve-datafusion` is what pulls the
//! DataFusion/Arrow tree on its own account, and a default build is DataFusion-free.)
//!
//! Environment:
//! - `VIKE_DATAHUB_ADDR` — listen address (default `127.0.0.1:7878`). ⚠ A NON-LOOPBACK address is
//!   REFUSED at startup unless `VIKE_DATAHUB_ALLOW_PUBLIC_BIND` is ALSO set to the exact string
//!   `"1"` **AND at least one node key is configured** — the tradehub `bind_decision` treatment,
//!   mirrored and then composed with this server's own key presence (see `crate::server`'s
//!   module doc, and the guard's own comment at the match below). The opt-in alone used to be
//!   enough, behind a `warn!`; that is the state `docs/decisions/0025-datahub-remote-posture.md`
//!   names verbatim as the one it exists to prevent. The guard did NOT soften when authentication
//!   landed: the handshake is plaintext and authenticates the CONNECTION, not each frame, so the
//!   tunnel is still what supplies confidentiality and integrity.
//!
//! Credential store (`<project>/settings/secrets.env`) — `docs/decisions/0025-datahub-remote-posture.md`:
//! - `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` — the scoped HMAC node keys. ⚠ **Their
//!   ABSENCE is the switch**: with neither set (the default) this server authenticates NOTHING and
//!   serves every verb that predates the keys exactly as every build before them did. With either
//!   set, EVERY connection must complete the `Hello`/`Auth` handshake, Observe reads history and
//!   the catalog, and Control additionally admits the `Backfill` WRITE and the `DeleteSeries`
//!   REMOVAL. ⚠ It used to say "and every `Run*` verb" — those verbs are the compute daemon's now
//!   (ruling 7), and the scope table that governs them moved with them to
//!   `vike_datahub_client::proto`'s `required_scope`, which BOTH daemons consult.
//!   those COMPILE CLIENT-SUPPLIED RHAI server-side, so they are not reads whatever they return.
//!
//!   ⚠ **The absence is no longer ONLY an authentication switch, and this bullet said it was until
//!   2026-09-07** ("behaves exactly as every build before them did"). It also decides one VERB: the
//!   destructive `DeleteSeries` is served by a KEYED server alone.
//!   `crates/vike-datahub/src/server.rs`'s `delete_series_verb` refuses it outright without keys
//!   and the `Welcome` does not advertise it, because `Scope::Write` is a word nothing enforces
//!   where nothing authenticates. `Backfill` is Control-scoped too and is NOT withheld for
//!   key-lessness — only `delete_series_verb` gates on `keyed` — so in a `backfill-serve` build
//!   with a table mounted it is served here. That asymmetry is the argument rather than an
//!   oversight, since a backfill writes rows a re-fetch restores and a removal takes the only copy.
//!   `docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md` is the record.
//! - the OANDA practice account's API token, in a `backfill-serve` build — the ONE venue credential
//!   this daemon reads, through a scoped read made per `Backfill` request and dropped with it
//!   (docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md;
//!   this file's `oanda_history_credentials` and `crate::backfill::credentialed_klines_row` carry
//!   the how).
//!   ⚠ On a KEY-LESS server the Control scope that fences it is a word nothing enforces — 0097
//!   declares that rather than gating it, for 0050's reason: the lane is what a local end user
//!   fetches with.
//! - `VIKE_DATAHUB_STORE` — hist-store root dir. Unset, it falls through the SHARED precedence that
//!   `resolve_store_root_from` documents at its call site below, which is the authority: the
//!   CWD-relative `market_data/hist` this line used to name as the last resort is gone, and it was the
//!   whole bug (launching from anywhere but the repo root created an empty store beside the shell).
//!
//! ⚠ `VIKE_USER_DATA_DIR` is NOT read here any more. It named the `indicators/` directory this
//! server installed process-wide so a CLIENT-SUPPLIED Rhai strategy could call the operator's own
//! indicators; ruling 7 moved every verb that compiles a strategy to `vike-backend backtest
//! --addr`, so this daemon compiles none and has nothing to install. The read went with the verbs
//! — `crates/vike-backtest/src/compute_server.rs`'s `install_user_indicators`.
//!
//! ⚠ **Since ruling 10 this binary also RECORDS**, and `--recorder-profile [NAME]` is the whole of
//! the new surface. `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0.5
//! merged `vike-recorder`'s daemon into this one — one process owning venue connections, the store
//! and serving — so `crate::recorder` is the mount and this file is where its flags are parsed and
//! its thread layout is decided. Read that module's doc before changing the order of anything below:
//! the recording keeps the MAIN thread and `serve_authed` moves to a spawned one, deliberately.
//!
//! ⚠ **`--record PATH` — the FILE spelling this surface shipped with — is RETIRED (decision 0086:
//! "settings live only in the database", verdict 1: no binary reads a profile TOML).** It is
//! accepted for one release, its VALUE ignored, and a startup warning fires — the exact
//! `RETIRED_PROFILE_FLAG` migration-window shape this file already used once for the flag's own
//! rename. See [`ProfileSource`]'s doc for the deployment hazard that shape exists to avoid and the
//! residual an operator must clear (a recorder profile ROW, via `vike-cli config
//! bootstrap-recorder`) before this release reaches a box that has only ever used `--record`.
//!
//! ⚠ **No new environment variable and no new settings key.** The profile arrives as a FLAG, the
//! way it did on the retired `vike-recorder --profile`, so a deployment moves one `ExecStart=` line
//! and nothing else — and `crates/vike-ops/tests/settings_registry.rs` and
//! `vike_config::CONSUMPTION` gain no row for a knob nobody asked for.
//!
//! ⚠ **`--help`/`--version` are answered in BOTH builds, identically, before anything else.** This
//! binary parsed NO argv at all: under the feature it went straight to opening a store and BINDING
//! A LISTENER, so `vike-datahub --help` STARTED A SERVER; without it, every invocation printed the
//! missing-feature message and exited 2, so `--help` looked like a first-class feature error when it
//! was simply not implemented. Help is a question about the COMMAND LINE, which both builds have;
//! the missing backend is a question about running, which is why only the RUN path still fails.

use std::process::ExitCode;

const USAGE: &str = "\
usage: vike-datahub [--record PATH | --recorder-profile [NAME]] [--tick-secs N] [--silent-secs N]
                    [--exit-on-silence] [--once]

The headless data daemon: it serves the hist store over the node protocol and, with a recording
profile, ALSO owns the venue subscriptions that fill it. The listen address and the store root come
from the environment; the recording profile is the one flag. (The COMPUTE verbs are NOT here — they
are served by `vike-backend backtest --addr`, and this daemon refuses them by name.)

  --recorder-profile [NAME]
                    record from the `recorder` profile row NAME in the settings store
                    (<project>/settings/db/vike.db). THE SHIPPED UNIT USES THIS. A name the
                    store does not hold is REFUSED before the listener binds — it is never a
                    daemon that comes up healthy and records nothing. `vike-cli config recorder`
                    lists what the store holds; if it holds none yet, `vike-cli config
                    bootstrap-recorder <name> --store <root> --venue <v> (--family <f>|--symbols
                    <a,b,c>)` builds one FROM ARGUMENTS (0086: never from a file) and activates it.
                    ⚠ With NO value it records whichever recorder profile is ACTIVE — the same
                    row `vike-cli` calls the default, resolved by one shared function so the two
                    sides cannot disagree. If none is active that is a REFUSAL naming the
                    profiles this store holds, not a silent serve-only start.
                    ⚠ RENAMED from `--record-profile`. That spelling STILL WORKS FOR THIS RELEASE
                    and warns at startup; the NEXT release refuses it and this daemon will not
                    start. Edit the unit's ExecStart= line now, then systemctl daemon-reload. The
                    object is called `recorder` everywhere else.
  --record PATH     RETIRED (0086): accepted for ONE release and the PATH is IGNORED — this
                    binary never opens it. Falls to whichever recorder profile is ACTIVE, the
                    same source a bare `--recorder-profile` names, behind a startup warning
                    naming the edit to make. A box that has only ever used `--record` needs a
                    recorder profile ROW before this release reaches it — see
                    `--recorder-profile`'s bootstrap line above. Giving both `--record` and
                    `--recorder-profile` is still a refusal; the operator names which.
                    Omitting both, this daemon serves and records nothing, exactly as it always did.
  --tick-secs N     how often to re-resolve the desired symbol set (default 30). Needs a profile.
  --silent-secs N   warn when a subscribed series has received no rows for N seconds
                    (default 300; 0 disables). Catches a feed that is subscribed and
                    connected but receiving nothing — which raises no error anywhere.
                    Needs a recording (--recorder-profile).
  --exit-on-silence stop with status 3 when a series is silent, so Restart=on-failure
                    can act. OFF by default: a venue can be legitimately quiet, and a
                    quiet market must not kill a daemon recording five healthy ones — and
                    since the merge it would take the data wire down with them. Alerting
                    (the [alerting] profile table) is the always-on reaction. Needs a recording
                    (--recorder-profile).
  --once            run ONE recording tick and exit — the dry run. Binds NO listener, so it is
                    safe to run beside the daemon it is checking. Exits 0 only if EVERY feed
                    ended that tick with at least one live subscription and no refused
                    subscribe; otherwise status 4, naming each feed that would record
                    nothing. Stricter than the daemon on purpose: this is a commissioning
                    check of your own profile, with you watching. Needs a recording
                    (--recorder-profile).
  -h, --help        print this and exit 0
  -V, --version     print the version and exit 0

environment:
  VIKE_DATAHUB_ADDR    listen address (default 127.0.0.1:7878). A non-loopback address is
                       refused at startup unless VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 is also set
                       AND a node key is configured (see the credential store below).
                       The handshake is plaintext even when keys are set, so it is meant to be
                       reached over an SSH tunnel (ssh -L 7878:localhost:7878 <box>).
  VIKE_DATAHUB_ALLOW_PUBLIC_BIND
                       the explicit opt-in for a non-loopback bind (the exact string 1). It
                       consents to being REACHABLE, and is not sufficient on its own: with no
                       node key configured the bind is still REFUSED, because the opt-in is not
                       consent to serve a store write and a store REMOVAL unauthenticated. With
                       a key it proceeds behind a logged warning naming what is exposed.

credential store (<project>/settings/secrets.env — `vike-cli secrets path` prints it):
  VIKE_DATAHUB_OBSERVE_KEY   read scope: history + catalog. Absent = no authentication at all.
  VIKE_DATAHUB_CONTROL_KEY   write scope: the above, plus the Backfill store write, the
                             DeleteSeries store REMOVAL. (The Run* verbs, which COMPILE
                             CLIENT-SUPPLIED RHAI, moved to `vike-backend backtest --addr`.)
                       With NEITHER set this server authenticates nothing and serves every verb
                       EXCEPT DeleteSeries to whoever can reach the socket — which is why that
                       build is CONFINED to a loopback address (see VIKE_DATAHUB_ADDR).
                       A key-less server neither advertises the delete verb nor answers one:
                       with nothing authenticating, its Control scope would be a word nothing
                       enforces, and unlike a backfill (whose rows a re-fetch restores) a
                       removal takes the only copy. Delete on the box instead, with
                       `vike-cli data hist rm --store DIR`.
                       Setting either key makes the handshake mandatory on every connection, and
                       is what makes the delete verb exist at all; reaching it additionally needs
                       the CONTROL key, since a server holding none refuses that scope outright.
  OANDA_DEMO_API_KEY   the practice account's OANDA API token — the ONE venue credential this
                       server reads (docs/decisions/0097), and only in a `backfill-serve` build.
                       A Backfill for oanda (Control scope) reads it when the request arrives and
                       drops it when the request ends, so storing, rotating or removing it needs
                       no restart; absent, that Backfill is refused naming this key.
                       No Observe verb reaches it, and a live-account key is never read. The
                       startup log says whether it is present, never its value.
  VIKE_DATAHUB_STORE   hist-store root; unset, falls through VIKE_HIST_STORE, a repo checkout if
                       this machine has one, this project's own market_data/hist, then a per-user dir
                       (VIKE_USER_DATA_DIR is no longer read: this daemon compiles no strategy.
                       The Run* verbs, and the indicator install they needed, moved to
                       `vike-backend backtest --addr`.)
  VIKE_DATAHUB_LIVE    the exact string \"1\" ARMS the LIVE MARKET-DATA plane: this daemon becomes
                       the single subscriber to each venue's book/depth/tape and pushes them to
                       clients over the MdSubscribe stream. UNSET = off, and off is
                       byte-identical to a build without the plane — no venue socket, no
                       `market_data` capability in Welcome.features, MdSubscribe refused by name.
                       ⚠ It spends VENUE-API budget from THIS BOX'S IP, shared with the
                       order-signing daemon, which is why it is an explicit operator act and why
                       the per-venue key cap and the 60 s linger exist.
                       Needs a build carrying `--features live-feeds` plus the `live-<venue>`
                       features for the venues you want; a build without them arms a hub that
                       refuses every venue BY NAME rather than serving nothing quietly.
  VIKE_DATAHUB_LIVE_RESIDENT
                       comma-separated `venue:symbol:lane` rows PINNED at startup (lane is one of
                       depth|book|trades), e.g. `binance:BTCUSDT.P:depth,binance:BTCUSDT.P:trades`.
                       A resident key has a refcount FLOOR of 1 and is never released, so a hot
                       symbol's ladder paints on the first DOM open instead of waiting on a REST
                       re-seed, and the daemon's traded pair stays observable with no desktop
                       attached. Unset = an EMPTY resident set, which is a real configuration:
                       the hub still serves on-demand keys.
  VIKE_DATAHUB_CHART_SEED
                       the exact string \"1\" ARMS the CHART-GAP SEED lane: a client whose chart has
                       no bars for a (venue, symbol, interval) may ask this daemon to fetch ONE
                       bounded window of klines into its own store. UNSET = off, and off is
                       byte-identical to a build without the lane except in one respect that is
                       deliberate: the verb is still ANSWERED, successfully, having fetched
                       nothing. (The `seed_series` capability is absent from Welcome.features
                       either way, so a client that reads the handshake names this variable to its
                       operator instead of drawing an empty chart.)
                       ⚠ It spends VENUE-API budget from THIS BOX'S IP, shared with the
                       order-signing daemon, and the client holds only an OBSERVE key — which is
                       why it is an explicit operator act. The server owns every bound: which
                       venues (this build's collector table), which intervals, how much history,
                       how often, and how many distinct series this process will ever seed. The
                       client names a series and nothing else.
                       Needs a build carrying `--features backfill-serve` (the same collectors
                       Request::Backfill uses — this lane adds no venue code of its own).
                       docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md is the record.
  (the VENUE-CATALOG lane is ON BY DEFAULT and is NOT an environment variable)
                       A client may ask this daemon for ONE venue's PUBLIC instrument list, and
                       since docs/decisions/0066 that is the DEFAULT: without an instrument list
                       you cannot pick a symbol, so a lane that was off by default meant the
                       product did not work out of the box.
                       ⚠ THE SWITCH IS ITS REFUSAL, and it is a SETTING rather than a variable:
                         vike-cli config set flags.venue_catalog_off true
                       or VIKE_DATAHUB_VENUE_CATALOG_OFF=1 in the environment. Refused, the verb
                       is still ANSWERED, successfully, having fetched nothing, and the
                       `venue_catalog` capability is absent from Welcome.features — so a client
                       that reads the handshake names the refusal to its operator instead of
                       showing an EMPTY symbol list, which for `ig` and `ibkr` would be
                       indistinguishable from the truth.
                       ⚠ VIKE_DATAHUB_VENUE_CATALOG (the old arming) configures NOTHING now. A
                       value that used to mean OFF is WARNED about at startup; `=1` is silent,
                       because the belief it expresses is still true.
                       ⚠ It spends VENUE-API budget from THIS BOX'S IP, shared with the
                       order-signing daemon, and the client holds only an OBSERVE key. The server
                       owns every bound: which venues (this build's provider table), how often
                       (a per-venue token bucket, keyed PER VENUE so an expensive venue spends its
                       own budget), how long an answer is reused (a TTL), and how many instruments
                       one listing may carry. None of those moved when the default did — they are
                       what bounds the cost, and the switch never did.
                       ⚠ It writes NOTHING to the store: no rows, no partition, nothing
                       ListSeries/Inventory/Coverage can see and nothing DeleteSeries can reach.
                       ⚠ No CREDENTIALED venue is reachable through it at any setting. alpaca,
                       oanda and ctrader can only be listed by authenticating as you, and the
                       provider table refuses them by construction rather than by a switch.
                       Needs a build carrying `--features catalog-serve` for a non-empty table;
                       without one, every venue answers `NotServed` and says so.
                       docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md
                       docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md

This binary serves only when built with the `serve-datafusion` feature, which is what makes the
concrete DataFusion backend nameable:
  cargo run -p vike-datahub --features serve-datafusion";

/// The recording half of the command line, as a plain value.
///
/// ⚠ **Parsed in BOTH builds, feature-free, and that is the point.** `--record` and its four
/// companions are a question about the COMMAND LINE, which every build has; whether this binary can
/// ACT on them is a question about the backend, which only a `record` build answers — the same
/// split `short_circuit` already drew for `--help`, and for the same reason (a build that cannot
/// serve still described its own command line correctly, and a build that cannot record must
/// describe its own too, then say so at RUN time rather than reject a flag it understands).
///
/// Named fields rather than `vike_datahub::recorder::RecordArgs` because that type lives behind the
/// feature and this parser must not.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordRequest {
    profile: ProfileSource,
    tick_secs: u64,
    silent_secs: u64,
    exit_on_silence: bool,
    once: bool,
    /// Set when the RETIRED flag spelling was used — logged once at startup, never silent.
    ///
    /// ⚠ It rides the parse rather than being printed inside it, because `parse_args` runs before
    /// `vike_log::init` and a `println!` there would land outside the JSON file layer every service
    /// manager captures. The same shape the credential store's own findings take a few hundred
    /// lines down: returned as DATA, logged by the caller once a subscriber exists.
    retired_flag_warning: Option<String>,
}

/// WHERE the recording profile comes from — **the settings database, always, since decision 0086
/// ("settings live only in the database") — never a file.**
///
/// ⚠ **This CHANGED on 0086, and the history is worth carrying rather than erasing.** Until then
/// this was a choice between a `File(PathBuf)` variant (`--record PATH`, read and parsed exactly as
/// it always was) and a database row, argued as "two SOURCES, never a precedence chain" so a
/// half-migrated box could not read half from each. 0086 verdict 1 forbids reading a profile TOML
/// from ANY binary, which removes one side of that choice outright — see [`Parsed`]'s caller for how
/// `--record PATH` is retired (accepted for one release, its VALUE ignored, exactly the
/// `RETIRED_PROFILE_FLAG` migration-window shape below) rather than refused, for the SAME deployment
/// hazard the old doc named: `deploy/sbin/vike-trader-ci-deploy` replaces the BINARY unattended on a
/// tag while a unit file is installed out of band by an operator, so a release that outright refused
/// `--record` would take a live recorder down on the next tag with an argument its own unit still
/// spells. Retiring it to a WARNING plus the active row is what closes that hazard rather than
/// reopening it with a new flag.
///
/// ⚠ **Residual an operator must clear before this lands on a box that has only ever used `--record
/// PATH`**: retiring the flag means falling to [`ProfileSource::ActiveRow`], and a box with no
/// recorder profile ROW yet REFUSES there (`crate::recorder::load_and_check_active_profile_row`'s
/// three noes) rather than recording from the file it used to. `vike-cli config bootstrap-recorder`
/// is the rung that builds a first row FROM ARGUMENTS (0086 forbids reading the old file to seed it)
/// — run it, with the old file's own store/subscriptions typed as arguments, BEFORE this release
/// reaches that box.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProfileSource {
    /// `--recorder-profile NAME` — a `recorder` profile row in `<project>/settings/db/vike.db`.
    Row(String),
    /// `--recorder-profile` with NO value — the row of kind `Recorder` carrying `active`, resolved
    /// through `vike_secrets::profile_store::Profiles::resolve_active`.
    ///
    /// ⚠ **It is a THIRD variant rather than a `Row(Option<String>)`, and the reason is what the
    /// two carry.** A named row is a string an operator typed and every message repeats back; this
    /// one carries nothing, because the name is not known until the store is read — and then it is
    /// the ROW's own name, which is what a journal must show. Collapsing them would make every
    /// match site ask "and if it is `None`?" at a point where it cannot answer.
    ///
    /// ⚠ It is NOT a precedence rung above [`ProfileSource::Row`]. The two are the same SOURCE (the
    /// settings database) reached by a resolution or by a name, exactly one of which was asked for;
    /// `ProfileSource`'s own doc governs both. Ruling 5 of
    /// `docs/superpowers/specs/2026-09-22-data-realtime-record-design.md`.
    ActiveRow,
}

/// How often the desired symbol set is re-resolved when no `--tick-secs` is given.
///
/// ⚠ Spelled here as well as in `crate::recorder` because this parser is feature-free and that
/// module is not — the two are held equal by [`the_parser_defaults_match_the_recorders`], which is
/// compiled only in a `record` build, i.e. exactly where both spellings exist.
const DEFAULT_TICK_SECS: u64 = 30;

/// The silence-watchdog grace when no `--silent-secs` is given. Same duplication, same gate.
const DEFAULT_SILENT_SECS: u64 = 300;

/// Naming BOTH profile flags is a refusal, not a precedence question — see [`ProfileSource`].
///
/// ⚠ **The wording used to say "a file and a database row", back when `--record PATH` genuinely
/// read one.** 0086 retires that: `--record` now IGNORES its path and resolves to the same
/// [`ProfileSource::ActiveRow`] a bare `--recorder-profile` does, so giving both is refused for the
/// same reason giving `--recorder-profile` twice would be — one profile, one flag naming it — not
/// because they reach two different STORES any more.
const REFUSE_TWO_SOURCES: &str = "--record and --recorder-profile both name a recording profile, so giving both is ambiguous \
     rather than a precedence question. Pick one — `--record` is RETIRED (0086) and its path is \
     ignored either way, so `--recorder-profile [NAME]` is the spelling to keep.";

/// The RETIRED spelling of [`RECORDER_PROFILE_FLAG`], refused BY NAME rather than dropped.
///
/// ⚠ **It stands on an `ExecStart=` line on two deployed boxes**, so a build that merely stopped
/// understanding it would fail with `unknown argument: --record-profile` — true, but it names
/// neither the new spelling nor the fact that a rename happened, and an operator reading that in a
/// journal has to go and find this file. A refusal that says what to write instead is the whole
/// difference between a five-second edit and an outage somebody debugs.
///
/// ⚠ **ACCEPTED FOR ONE RELEASE, behind a startup warning — and then refused.** The first version
/// of this constant refused it outright, on the no-alias rule. That rule is not in question and the
/// destination is unchanged; what defeated the refusal is a DEPLOY ORDERING fact, measured on
/// the CI box 2026-09-22:
///
/// * the live unit spells this flag;
/// * `/usr/local/sbin/vike-trader-ci-deploy` installs the BINARY and never the unit — its own words
///   are that `/etc/systemd/system` is *"replaced OUT OF BAND"* — and it runs no `daemon-reload`;
/// * so a refusal fails in BOTH orders. Edit the unit first and the CURRENT binary refuses the new
///   spelling at its next restart, which `unattended-upgrades` performs daily. Ship first and the
///   new binary refuses the live unit, the health check fails, and the deploy auto-rolls-back.
///
/// A migration window with a date on it is not a second name to keep: it is how a renamed flag
/// reaches a box that nothing else updates. **The refusal returns in the release after every unit
/// carries [`RECORDER_PROFILE_FLAG`]**, and the warning this arm emits is the instruction to make
/// that edit.
const RETIRED_PROFILE_FLAG: &str = "--record-profile";

/// The flag that names the `recorder` profile row — `--recorder-profile [NAME]`.
///
/// ⚠ **Spelled as a constant because the DEPLOY GATE reads it.**
/// `crates/vike-ops/tests/deploy_layout_gate.rs`'s `records` decides whether a shipped unit WRITES
/// the store by looking for this flag on `ExecStart=`, and gates the store's `ReadWritePaths=`
/// grant in BOTH directions off that answer. A unit that dropped the flag would read as
/// non-recording, be required to drop its grant, and then start, subscribe, buffer and die at the
/// first flush with the wire already up — which is why the value is OPTIONAL rather than the flag.
///
/// ⚠ **`record` -> `recorder` because five of the six sites naming this object already said
/// `recorder`** and this flag was the only holdout: `ProfileKind::Recorder`,
/// `<project>/settings/recorder.toml`, `vike-cli config recorder`, `vike-cli config mirror
/// --recorder`. `record` survives as the IMPERATIVE VERB in `vike-cli data realtime record add` —
/// a different part of speech, not a second name for the object.
const RECORDER_PROFILE_FLAG: &str = "--recorder-profile";

/// What a successful parse produced. Named variants rather than a bool pair, the shape
/// `vike-tradehub`'s `Parsed` uses — nothing here can be confused for a run.
///
/// `Debug` so a parse that was supposed to FAIL can report what it actually produced instead
/// (`Result::expect_err` requires it) — the whole value of the unknown-argument test below.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Parsed {
    /// Serve, and record if a profile was named.
    Run(Option<RecordRequest>),
    Help,
    Version,
}

/// The parser's job is the three things every binary owes a caller — answer `-h`/`--help`, answer
/// `-V`/`--version`, and REJECT anything else instead of ignoring it (a typo in a systemd
/// `ExecStart=` used to start the server anyway) — plus, since ruling 10 merged the recorder in,
/// the five flags that name a recording.
///
/// ⚠ **It used to be loop-free**, because there was nothing to accumulate: "the FIRST argument
/// already decides all three outcomes, and anything after it is unreachable by construction". That
/// stopped being true the moment a flag took a VALUE, so the loop is back — and with it the rule
/// the recorder's own parser carried: a flag that needs an argument and does not get one is an
/// error, never a silently-defaulted knob.
///
/// ⚠ A companion flag without `--record` is REFUSED rather than ignored. `--once` alone would
/// otherwise start a server, and `--silent-secs 0` alone would leave an operator believing they had
/// turned a watchdog off on a daemon that was never watching anything.
fn parse_args(argv: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut profile: Option<ProfileSource> = None;
    let mut retired_flag_warning: Option<String> = None;
    let mut tick_secs = DEFAULT_TICK_SECS;
    let mut silent_secs = DEFAULT_SILENT_SECS;
    let mut exit_on_silence = false;
    let mut once = false;
    let mut companion: Option<&'static str> = None;
    // PEEKABLE since 2026-09-22, and only one flag needs it: `--recorder-profile`'s value is
    // OPTIONAL, so the parser has to LOOK at the next token without consuming it. Every other flag
    // here takes its value unconditionally and `next()` is still the right call for them.
    let mut it = argv.peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "--record" => {
                // ⚠ RETIRED (0086): the PATH is taken so the flag still parses on an unedited
                // unit, then IGNORED — never opened, never read as a file. See [`ProfileSource`]'s
                // doc for why this is a warned migration window rather than an outright refusal.
                let v = it.next().ok_or("--record needs a path".to_string())?;
                if profile.is_some() {
                    return Err(REFUSE_TWO_SOURCES.to_string());
                }
                profile = Some(ProfileSource::ActiveRow);
                retired_flag_warning = Some(format!(
                    "--record {v} is RETIRED (decision 0086 — settings live only in the \
                     database; no binary reads a profile TOML). The PATH is IGNORED for this \
                     release: this daemon records whichever recorder profile is ACTIVE instead, \
                     the same source a bare `{RECORDER_PROFILE_FLAG}` names explicitly. Edit \
                     this unit's ExecStart= line to `{RECORDER_PROFILE_FLAG} <name>` (or \
                     `{RECORDER_PROFILE_FLAG}` alone for the active row) now — the NEXT release \
                     refuses `--record` outright and this daemon will not start with it. \
                     `vike-cli config recorder` lists what this store holds; if it holds none \
                     yet, `vike-cli config bootstrap-recorder <name> --store <root> --venue <v> \
                     (--family <f>|--symbols <a,b,c>)` builds one FROM ARGUMENTS and activates \
                     it — do this BEFORE this release reaches a box that has only ever used \
                     `--record {v}`."
                ));
            }
            RECORDER_PROFILE_FLAG => {
                if profile.is_some() {
                    return Err(REFUSE_TWO_SOURCES.to_string());
                }
                // ⚠ **The OPTIONAL value, and the rule is "a token that is not another flag".**
                // A bare `--recorder-profile` means the ACTIVE row (ruling 5); a following token
                // that starts with `-` belongs to the next flag and must NOT be eaten, or
                // `--recorder-profile --once` would silently record a profile named `--once` and
                // then refuse it as unknown. A profile name that genuinely starts with `-` is
                // therefore unreachable through this flag — an accepted cost, and the same one
                // every optional-value flag in every CLI pays.
                let named = it.peek().is_some_and(|v| !v.starts_with('-'));
                profile = Some(if named {
                    ProfileSource::Row(it.next().unwrap_or_default())
                } else {
                    ProfileSource::ActiveRow
                });
            }
            RETIRED_PROFILE_FLAG => {
                // ⚠ ACCEPTED FOR ONE RELEASE, LOUDLY — and the reason is a DEPLOY ORDERING one
                // that a refusal cannot solve. MEASURED 2026-09-22 on the CI box: the live unit spells
                // the retired flag, `/usr/local/sbin/vike-trader-ci-deploy` installs the BINARY and
                // never the unit (it says so itself: "/etc/systemd/system is replaced OUT OF BAND"),
                // and it runs no `daemon-reload`. So a refusal makes BOTH orders fail — edit the
                // unit first and the CURRENT binary refuses the new spelling at its next restart
                // (`unattended-upgrades` performs one daily); ship first and the new binary refuses
                // the old unit, the health check fails and the deploy auto-rolls-back.
                //
                // Accepting it is therefore the only order-free route, and it does NOT reopen the
                // no-alias rule: the spelling is not an alias to keep, it is a MIGRATION WINDOW with
                // an owner's date on it. The refusal above is restored in the release AFTER every
                // unit carries `--recorder-profile` — that edit is the whole cost of this branch.
                let named = it.peek().is_some_and(|v| !v.starts_with('-'));
                profile = Some(if named {
                    ProfileSource::Row(it.next().unwrap_or_default())
                } else {
                    ProfileSource::ActiveRow
                });
                retired_flag_warning = Some(format!(
                    "{RETIRED_PROFILE_FLAG} was RENAMED to `{RECORDER_PROFILE_FLAG}` and is \
                     ACCEPTED FOR THIS RELEASE ONLY. Edit this unit's ExecStart= line now: \
                     `{RECORDER_PROFILE_FLAG} <name>` names a profile and \
                     `{RECORDER_PROFILE_FLAG}` with no value records whichever recorder profile is \
                     ACTIVE — then `systemctl daemon-reload`. The next release REFUSES the old \
                     spelling and this daemon will not start. The object was already called \
                     `recorder` everywhere else (settings/recorder.toml, `vike-cli config \
                     recorder`, `vike-cli config mirror --recorder`); this flag was the last holdout"
                ));
            }
            "--tick-secs" => {
                let v = it.next().ok_or("--tick-secs needs a number".to_string())?;
                let n: u64 = v.parse().map_err(|_| format!("--tick-secs: not a number: {v}"))?;
                if n == 0 {
                    // A zero tick would spin the resolver flat out against the venue's directory API.
                    return Err("--tick-secs must be > 0".into());
                }
                tick_secs = n;
                companion = Some("--tick-secs");
            }
            "--silent-secs" => {
                let v = it.next().ok_or("--silent-secs needs a number".to_string())?;
                silent_secs = v.parse().map_err(|_| format!("--silent-secs: not a number: {v}"))?;
                companion = Some("--silent-secs");
            }
            "--exit-on-silence" => {
                exit_on_silence = true;
                companion = Some("--exit-on-silence");
            }
            "--once" => {
                once = true;
                companion = Some("--once");
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let Some(profile) = profile else {
        return match companion {
            Some(flag) => Err(format!(
                "{flag} configures a recording, so it needs --record PATH or \
                 {RECORDER_PROFILE_FLAG} [NAME]. Without one this daemon only serves the store"
            )),
            None => Ok(Parsed::Run(None)),
        };
    };
    if exit_on_silence && silent_secs == 0 {
        // `--silent-secs 0` disables detection outright, so the exit could never fire — an
        // operator who wrote both believes something is armed that is not.
        return Err("--exit-on-silence needs silence detection on (--silent-secs > 0)".into());
    }
    Ok(Parsed::Run(Some(RecordRequest {
        profile,
        tick_secs,
        silent_secs,
        exit_on_silence,
        once,
        retired_flag_warning,
    })))
}

/// Answer `--help`, `--version` and a usage error IDENTICALLY in both builds — `Err(code)` means
/// the process is done. Shared by the two `main`s below so the feature can never change what a
/// caller's `--help` does; `Ok` carries the recording the caller asked for, if any.
fn short_circuit(args: &[String]) -> Result<Option<RecordRequest>, ExitCode> {
    match parse_args(args.iter().cloned()) {
        // Help is normal output a user pipes into a pager, so STDOUT and exit 0 — a non-zero
        // `--help` breaks `set -e`, packaging smoke tests and any wrapper that checks a status.
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            Err(ExitCode::SUCCESS)
        }
        // `<name> <version> (<build identity>)` — the shape every `--version` on the box prints
        // (`git version 2.x`), with the COMMIT this binary was built from appended. This server is
        // deployed on the CI box and reached over an SSH tunnel, so "is the running binary the code I
        // pushed?" is asked of it from a terminal; `crates/vike-buildinfo/src/lib.rs` carries the
        // near-miss that made a bare version number insufficient.
        Ok(Parsed::Version) => {
            println!(
                "{}",
                vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
            );
            Err(ExitCode::SUCCESS)
        }
        Ok(Parsed::Run(record)) => Ok(record),
        Err(e) => {
            eprintln!("vike-datahub: {e}\n\n{USAGE}");
            Err(ExitCode::from(2))
        }
    }
}

// ⚠ `install_user_indicators` is GONE, and its absence is a POSTURE change worth naming rather than
// a deletion. This binary used to load `<project>/user_data/indicators/*.rhai` and install them
// process-wide, because it COMPILED CLIENT-SUPPLIED RHAI: `RunBacktest` resolved a profile's
// `[strategy.params].src` through `harness::registry`, and `RunSlice` went through
// `wire_run::to_strategy_spec`. Ruling 7 moved every one of those verbs to
// `vike-backend backtest --addr`, so this daemon compiles no strategy at all — and a server that
// compiles nothing has no reason to hold a script compiler, a `vike-script` edge or an indicator
// set whose contents a client cannot see. The whole apparatus (and the ⚠⚠ "the server's set wins
// and the client cannot see it" hazard it carried) moved with the verbs to
// `crates/vike-backtest/src/compute_server.rs`, which is where it now belongs.

/// **Declare Polymarket's egress into the bridge** (decision 0095 — it opens no store and reads no
/// environment).
///
/// `loaded` is the ONE read of the `venue_setting` table [`run`] makes (`None`: no settings
/// directory); [`venue_settings_of`] takes the broker's copy from the same read. Everything that
/// decides what the rows MEAN for egress — the precedence, the ONE store-error policy, the warning
/// about a credential row nobody moved — lives in `vike_polymarket::declare_from_rows`, which the
/// trading daemon calls too: five copies of this body once handled an unreadable store two
/// different ways.
///
/// Gated to match its one call site's own `#[cfg]` exactly (inside [`run`], itself
/// `serve-datafusion`-only, under the additional `any(venue-polymarket, catalog-serve)` the call
/// carries) — a looser gate would leave this function uncalled, and therefore unused, in a build
/// that compiles `run` without either venue feature.
///
/// See [`polymarket_egress_credentials`] for why this root reads the credential store at all.
#[cfg(all(
    feature = "serve-datafusion",
    any(feature = "venue-polymarket", feature = "catalog-serve")
))]
fn declare_polymarket_egress(
    settings_dir: Option<&std::path::Path>,
    loaded: Option<
        Result<
            std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
            vike_secrets::DbError,
        >,
    >,
) {
    let credentials = settings_dir.and_then(polymarket_egress_credentials);
    vike_polymarket::declare_from_rows(loaded, credentials.as_ref());
}

/// Every venue's `venue_setting` rows as the broker's client table reads them — Polymarket's socket
/// batching, each CEX feed's mark-stream row — taken from [`run`]'s ONE read of the table (`None`:
/// no settings directory). Empty for no directory, no database or no rows.
///
/// A store that will not open is logged HERE for what it costs the feed clients (every venue takes
/// its built-in defaults), and reads as empty. Polymarket's egress takes the same read through
/// `declare_polymarket_egress` (compiled only where Polymarket's egress is), whose shared policy
/// logs the egress half — so a build linking Polymarket says both, and one that does not still
/// says this one.
#[cfg(feature = "serve-datafusion")]
fn venue_settings_of(
    loaded: Option<
        &Result<
            std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
            vike_secrets::DbError,
        >,
    >,
) -> std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings> {
    match loaded {
        Some(Ok(all)) => all.clone(),
        Some(Err(e)) => {
            tracing::error!(
                error = %e,
                "the venue_setting table could not be read; every venue's market feed takes its \
                 built-in defaults (each venue's charter mark-stream default, the default socket \
                 batching)"
            );
            std::collections::BTreeMap::new()
        }
        None => std::collections::BTreeMap::new(),
    }
}

/// **The credential store's values for Polymarket's five egress names — and ONLY those.**
///
/// ⚠ **Why this root reads the credential store at all.** This boot loads no credential map
/// ([`vike_boot::Credentials::Deferred`]; the node keys below come from the NODE store), yet the
/// deleted bridge reader honoured `POLY_PROXY_HOST` and its siblings from the credential store, and
/// a box that never ran `vike-cli secrets move-venue-config` (or keeps a file store) still holds
/// those as `credential` rows. The reach is exactly the one the bridge's own resolver had — the same
/// scoped read (`resolve_store_scoped_in`) — moved to the root that now owns it. It ends with the
/// fallback it serves (`vike_polymarket::declare_from_rows`'s ⚠ on step 2).
///
/// ⚠ **What "scoped" guarantees depends on the store, and is not "never materialises a venue key" on
/// both.** On a settings DATABASE the read binds the five declared names and selects no other row, so
/// no other credential ever enters this process. On a FILE store (`secrets.env`, no database) there
/// is nothing to bind: `resolve_store_scoped_in` reads and parses the WHOLE file and narrows
/// afterwards, so every value in it is transiently in memory. What this function RETURNS is only the
/// five names' values, and it logs no value at all (the permission finding and an error's `Display`
/// carry paths and reasons, never row contents).
/// `the_egress_credential_read_holds_the_five_names_and_no_venue_key` pins what comes back, over the
/// file arm.
///
/// ⚠ **The scoped read folds `venue_setting` rows into the map under the legacy names**, and on a
/// name held by both the credential value wins IN THE MAP — which is why the shared function
/// consults the rows first and the map second, and why nothing here filters the two.
///
/// `None` is an unreadable store, already logged; a store with none of the five is `Some(empty)`.
#[cfg(all(
    feature = "serve-datafusion",
    any(feature = "venue-polymarket", feature = "catalog-serve")
))]
fn polymarket_egress_credentials(
    settings_dir: &std::path::Path,
) -> Option<std::collections::HashMap<String, String>> {
    let scope = vike_secrets::KeyScope::of(vike_polymarket::egress_legacy_names());
    match vike_secrets::resolve_store_scoped_in(
        settings_dir,
        vike_secrets::Table::Credential,
        &scope,
    ) {
        Ok(scoped) => {
            // The store's permission finding, surfaced here because this root does not go through
            // `vike_bridge_core::credentials` (which logs it) — a credential file readable by
            // others is not something to read in silence. Names and paths only.
            if let Some(w) = &scoped.warning {
                tracing::warn!("{w}");
            }
            Some(scoped.into_map())
        }
        Err(e) => {
            // The same store the rows are read from, so the shared function reports the rows' half
            // of this; this line is the credential half, and both say what happens next.
            tracing::error!(
                error = %e,
                "the credential store could not be read for Polymarket's five egress names; any \
                 of them held only there is NOT honoured"
            );
            None
        }
    }
}

/// **The OANDA history lane's ONE credential, read out of the store** —
/// docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md,
/// verdict 2. The scope is `vike_oanda::oanda_history_token_names()`: the bridge names its own key
/// (the practice tier's API key, composed through the one site that composes every OANDA name), and
/// this root declares exactly that and nothing else — the shape [`polymarket_egress_credentials`]
/// has for its five names.
///
/// ⚠ **What "one name" guarantees depends on the store, as it does there.** On a settings DATABASE
/// the read binds the one declared name and selects no other row, so no other credential enters
/// this process. On a FILE store the whole file is parsed and narrowed afterwards — every value in
/// it is transiently in memory, and only the one name survives into what this RETURNS. That is the
/// scoped read's own declared caveat (`crates/vike-secrets/src/store.rs`'s `ScopedSecrets`), and
/// `the_oanda_history_read_holds_the_one_practice_key_and_nothing_else` pins what comes back over
/// BOTH arms.
///
/// ⚠ **`announce` is whether the store's permission FINDING is logged, and only the startup read
/// announces.** The lane's provider calls this once per `Backfill` request for oanda, and a finding
/// repeated per request would bury the log it is meant to be read in — the provider logs NOTHING on
/// the path that finds a store. A store that exists and cannot be read is logged either way: that
/// read ends the request, so it is one line per failed request, and the refusal the operator reads
/// (`vike_oanda::HistoryTokenError::StoreUnreadable`'s) sends them to this log for the cause.
///
/// `None` is that unreadable store — never folded into "no credential", because a permissions bug
/// wearing the unconfigured answer would send the operator to store a key they already stored. A
/// store with no such key is `Some` without it. No line this function logs carries a value: the
/// permission finding and the error's `Display` carry paths, modes and OS reasons.
#[cfg(feature = "backfill-serve")]
fn oanda_history_credentials(
    settings_dir: &std::path::Path,
    announce: bool,
) -> Option<std::collections::HashMap<String, String>> {
    let scope = vike_secrets::KeyScope::of(vike_oanda::oanda_history_token_names());
    match vike_secrets::resolve_store_scoped_in(
        settings_dir,
        vike_secrets::Table::Credential,
        &scope,
    ) {
        Ok(scoped) => {
            // The store's permission finding, surfaced for the reason the egress read gives: this
            // root does not go through `vike_bridge_core::credentials`, which is what logs it for
            // the other roots, and a credential file readable by others is not read in silence.
            if announce && let Some(w) = &scoped.warning {
                tracing::warn!("{w}");
            }
            // `into_map` ends the three-state answer on purpose: the scope IS the point here, and
            // the reader it feeds (`vike_oanda::load_oanda_history_token`) is the bridge's own
            // fixed-name reader over the names the bridge itself declared.
            Some(scoped.into_map())
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "the credential store could not be read for the OANDA history lane's practice \
                 token; a Backfill for oanda is refused, and it is NOT the same as the key being \
                 absent"
            );
            None
        }
    }
}

/// **One read of the practice token** — the three states 0097's verdict 6 names, each its own
/// answer: `Ok` (present), [`vike_oanda::HistoryTokenError::NotConfigured`] (absent from a store
/// that answered — or no settings directory at all, so no store to hold it) and
/// [`vike_oanda::HistoryTokenError::StoreUnreadable`]. The scoped map is dropped before this
/// returns; the `String` is the only copy that leaves.
#[cfg(feature = "backfill-serve")]
fn read_oanda_history_token(
    settings_dir: Option<&std::path::Path>,
    announce: bool,
) -> Result<String, vike_oanda::HistoryTokenError> {
    let Some(dir) = settings_dir else {
        return Err(vike_oanda::HistoryTokenError::NotConfigured);
    };
    let credentials = oanda_history_credentials(dir, announce)
        .ok_or(vike_oanda::HistoryTokenError::StoreUnreadable)?;
    vike_oanda::load_oanda_history_token(&credentials)
        .ok_or(vike_oanda::HistoryTokenError::NotConfigured)
}

/// **The token PROVIDER `crate::backfill::real_backfill_table` builds the OANDA row around** — a
/// closure over the settings DIRECTORY, which is all it holds. Every call is a fresh, silent scoped
/// read ([`read_oanda_history_token`]). The row asks it at most ONCE per request —
/// `crate::backfill::credentialed_klines_row` builds each request's source around a read-once memo,
/// the contract `vike_oanda::HistoryTokenProvider`'s own doc sets — and nothing here caches a value
/// between calls, so a token stored, rotated or removed after this daemon started is what the NEXT
/// request sees: 0097's "removing the token disarms the lane", with no restart either way.
#[cfg(feature = "backfill-serve")]
fn oanda_history_token_reader(
    settings_dir: Option<std::path::PathBuf>,
) -> vike_oanda::HistoryTokenProvider {
    std::sync::Arc::new(move || read_oanda_history_token(settings_dir.as_deref(), false))
}

/// The startup line that says whether the OANDA history lane is ARMED — 0097's verdict 6: the log
/// says it, and never says a value. `presence` is one read's answer with the token already dropped
/// (`Result<(), _>`), so no value can reach this text by construction; it names the KEY, from the
/// bridge's own declaration rather than a literal.
///
/// Arming is the act of storing the key: there is no second switch, and the text says that the key
/// is read per request so an operator does not restart for it.
#[cfg(feature = "backfill-serve")]
fn oanda_history_lane_line(
    settings_dir: Option<&std::path::Path>,
    presence: Result<(), vike_oanda::HistoryTokenError>,
) -> String {
    let key = vike_oanda::oanda_history_token_names().join(", ");
    match (settings_dir, presence) {
        (_, Ok(())) => format!(
            "oanda history lane: ARMED — {key}, the practice account's API token, is present in \
             this server's credential store. It is read when a Control-scope Backfill for oanda \
             arrives and dropped when that request ends; nothing holds it in between, and no \
             Observe verb reaches the lane (docs/decisions/0097)"
        ),
        (None, Err(_)) => format!(
            "oanda history lane: not armed — this server resolved no settings directory, so there \
             is no credential store to hold {key}, and a Backfill for oanda is refused"
        ),
        (Some(_), Err(vike_oanda::HistoryTokenError::StoreUnreadable)) => format!(
            "oanda history lane: UNKNOWN — the credential store could not be read (the error above \
             names it), so whether {key} is stored cannot be told, and a Backfill for oanda is \
             refused until it can be. This is not the same as the key being absent"
        ),
        (Some(_), Err(vike_oanda::HistoryTokenError::NotConfigured)) => format!(
            "oanda history lane: not armed — {key}, the practice account's API token, is absent \
             from this server's credential store, so a Backfill for oanda is refused naming the \
             fix. `vike-cli secrets set {key}` on THIS box arms it; the key is read per request, \
             so there is nothing to restart"
        ),
    }
}

#[cfg(feature = "serve-datafusion")]
pub fn run(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&std::path::Path>,
    args: &[String],
) -> std::process::ExitCode {
    use std::net::{TcpListener, ToSocketAddrs};
    use std::sync::Arc;

    // BEFORE the log subscriber, the store and the listener: `--help` must not open a store, and it
    // certainly must not bind a port. (It did both — this binary read no argv at all.)
    let record = match short_circuit(args) {
        Ok(r) => r,
        Err(code) => return code,
    };
    // ⚠ A `--record` on a build without the `record` feature is a RUN failure naming the feature,
    // never a silent serve-only start. This is `crate::recording::build_recording_feed`'s rule one
    // level up: "a startup ERROR naming the missing feature, never a silent no-record", and it
    // matters more here — the daemon would otherwise come up healthy, answer every query, and accumulate
    // nothing, which is indistinguishable from a venue that is merely quiet.
    #[cfg(not(feature = "record"))]
    if record.is_some() {
        eprintln!(
            "vike-datahub: --record was given but this binary was built without the `record` \
             feature, so it carries no venue feeds and would serve a store nothing fills. Rebuild \
             with: cargo build -p vike-datahub --features record-polymarket,record-binance (or \
             `record` alone for the mount with no venue compiled in)"
        );
        return ExitCode::from(2);
    }

    // ONE environment sweep for this whole root, threaded to every reader below — the shape
    // `vike-desktop`'s `PROCESS_ENV` and `vike-cli`'s `resolve_policy` use. Two sweeps could
    // disagree if anything mutated the environment between them, and a composition root is the one
    // place that question should have a single answer.

    // The workspace's ONE startup sequence. This root uses the narrowest slice of it there is — the
    // identity line and the log home — and every way it departs is a named arm below, which is the
    // point: those departures used to be prose in this comment block and nothing could see them.
    let booted = match vike_boot::boot(&vike_boot::BootSpec {
        env: vars,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        // ⚠ REFUSE since decision 0095. The `Ignore` this replaced argued that the removed variables
        // were ceilings this server never read; REMOVED_ENV now also carries the Polymarket egress
        // variables it DID read, and ignoring a leftover `POLY_PROXY_ENABLED=false` would put it back
        // on the default proxy in silence — the change the spec's D3 refuses. A stale ceiling
        // variable now stops this server too; the release's deploy pre-flight names every such line
        // before anything is installed.
        removed_env: vike_boot::RemovedEnv::Refuse,
        // ⚠ **This was `SettingsLoad::Skip` until `docs/decisions/0066`, and the JOIN is that
        // record's decision 3 rather than a tidy-up.** The old reason ("this crate loads no
        // `vike_config::Settings` at all … It joins when it reads them") predicted its own end,
        // and `flags.venue_catalog_off` is what ended it: the venue-catalog lane is ON by default
        // now, so its REFUSAL is the only switch, and a refusal living in a file this process never
        // opened would be a default-on behaviour with NO off switch — precisely the state
        // `vike_config::Flags`' `*_off` pair exists to make impossible. The record rules the
        // ordering explicitly: the root joins the settings load, and the default flips with it or
        // after it, never before.
        //
        // What the join costs, stated rather than discovered: this daemon now reads
        // `<project>/settings/*.toml` and the mirrored settings-database rows, so a settings tree
        // that is present and BROKEN is a startup failure here as it already is at every other
        // root. That is the same disposition `vike-tradehub` and `vike-cli` have, and the loader's
        // own split (a store that will not OPEN degrades; a row that is ILLEGAL refuses) is
        // unchanged.
        //
        // It still reads its store root and address from `VIKE_DATAHUB_STORE`/`VIKE_HIST_STORE`/
        // `VIKE_DATAHUB_ADDR` — those are not settings keys and this change does not make them one.
        // ⚠ REFUSE, the same arm as `vike-tradehub` and for the same reason one rung down: this
        // daemon's own `config.*` and `flags.*` decide what it records and where it writes, and a
        // recorder running on a resolution nothing can vouch for writes a tape nobody can trust.
        // Its deployed unit carries an `ExecStartPre=vike-cli config check`, so on that box the
        // refusal is met before the process starts rather than as a restart loop.
        settings: vike_boot::SettingsLoad::Load,
        ceilings: vike_boot::Ceilings::NotInterpreted(
            "this server mounts no venue and reads no arming ceiling, and its unit cannot write \
             settings/db — a migration attempt here could only fail",
        ),

        // ⚠ The REASON here changed when `docs/decisions/0025-datahub-remote-posture.md` was
        // adopted; the ARM did not, and that is deliberate. It used to read "this server
        // authenticates nothing and mounts no venue", which is now false in its first clause — this
        // server CAN authenticate, and its node keys live in the credential store.
        //
        // It stays `Deferred` because the departure is about TIMING, not need: the keys are read a
        // few lines below, from `booted.settings_dir_override` — the answer THIS BOOT's one project
        // walk produced — rather than from a walk of this binary's own. That is the whole point of
        // the consolidation `vike_ops::settings`'s `VIKE_SETTINGS_DIR` block describes (four
        // composition-root reads became one under `vike-boot`), and taking `LoadWith` would mean
        // this root reading `$VIKE_SETTINGS_DIR` for itself to feed a loader — a fifth walk, and a
        // fifth chance for the log home, the store root and the credentials to answer with three
        // different projects.
        //
        // What is genuinely skipped with `Deferred` is `refuse_credential_file_arming`. That is
        // correct HERE and nowhere near a general licence: it refuses a store that arms REAL MONEY,
        // and this root mounts no venue, holds no `ExecutionClient` and can place no order, so it
        // has nothing to arm. The root that CAN — `vike-tradehub`; `vike-app` could too, until the
        // desktop lost its mount — takes `LoadWith` and runs it against the same file on the same
        // box.
        credentials: vike_boot::Credentials::Deferred(
            "this server mounts no venue and can place no order, so it has no real-money arming to \
             refuse. It DOES read the credential store — for its own node keys — but a few lines \
             later, off this boot's own `settings_dir_override`, so the project walk stays the one \
             this crate performed rather than a second one of the binary's.",
        ),
        log_home: vike_boot::LogHome::UnderSettings,
        // ⚠ The REASON here changed with the settings join above and the ARM did not, which is the
        // shape this file uses everywhere: it used to read "there are no settings to disclose",
        // and after `docs/decisions/0066` there is exactly one. `vike_config::boot_lines` performs
        // a SECOND read of the settings files to recover each row's ORIGIN and renders the RISK
        // CEILINGS — none of which this server reads, mounts a venue for, or can act on. What it
        // does consume discloses ITSELF, loudly, at the gate line below
        // (`vike_catalog::venue_catalog_gate_line`, which fires whichever way the verdict went), so
        // the one setting is in the log either way and a whole-tree disclosure would only add
        // ceilings this process cannot honour.
        disclosure: vike_boot::Disclosure::Skip(
            "the one setting this server consumes (`flags.venue_catalog_off`) discloses itself at \
             the venue-catalog gate line, whichever way the verdict went. A whole-tree disclosure \
             would describe the risk ceilings, which this server neither reads nor could act on.",
        ),
    }) {
        Ok(b) => b,
        // Unreachable with the arms above (nothing here can fail), but a refusal must never become
        // an unwrap in a server's `main`.
        Err(e) => {
            eprintln!("vike-datahub: {e}");
            return ExitCode::from(2);
        }
    };

    // Hold the appender guards for the process lifetime (dropping flushes the non-blocking writer).
    // `project_dir`: default the rolling trace file to `<project>/settings/state/logs` rather than
    // vike-log's `<exe_dir>/logs` last resort — this server runs from its project root under systemd,
    // where "beside the binary" is a directory nobody opens. `$VIKE_LOG_DIR` still wins.
    //
    // ⚠ The home now comes off the boot's OWN walk, which honours `$VIKE_SETTINGS_DIR`; the
    // `project_log_dir(&cwd)` call that stood here did not. `deploy/vike-datahub.service` sets BOTH
    // `WorkingDirectory=/srv/vike-<unit>` and `Environment=VIKE_SETTINGS_DIR=/srv/vike-<unit>/settings`,
    // so on the shipped deployment the two resolve to the same directory and nothing moves. Where
    // they DIFFER the old answer was the wrong one — the same defect `vike-recorder` had, where the
    // variable was set by every unit, claimed by both runbooks, and read by nothing.
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "vike-datahub".to_string(),
        project_dir: booted.log_home.clone(),
        ..Default::default()
    });

    // WHICH BINARY IS THIS — the first line in the log, and the same string `--version` prints, so
    // the answer is readable from the log alone and from the binary alone and the two cannot
    // disagree. This server is deployed and long-lived; `crates/vike-buildinfo/src/lib.rs` carries
    // the release binary built four commits behind `main` that motivated it.
    tracing::info!("{}", booted.identity_line);

    // ...AND WHICH MODE IT IS IN, in the same breath. See `startup_mode_line` for the silent
    // no-op this exists to make impossible to ship again.
    tracing::info!("{}", startup_mode_line(record.as_ref()));

    // The loader's OWN non-fatal resolutions, emitted here because `vike_boot::boot` runs before a
    // subscriber exists and therefore returns them as DATA. This root had none to emit while it
    // took `SettingsLoad::Skip` (the arm returns `Settings::default()`, whose `warnings` are
    // empty); the join above is what gives it some, and the one an operator of THIS daemon is most
    // likely to see is `vike_config::flags::venue_catalog_refusal_ignored` — the notice that a
    // `VIKE_DATAHUB_VENUE_CATALOG` they exported with a value that used to mean OFF no longer
    // turns anything off. Dropping them would make that notice unreachable on the only box it is
    // about.
    for w in &booted.settings.warnings {
        tracing::warn!("{w}");
    }

    // Decision 0095: every venue's `venue_setting` rows, read ONCE from the boot's settings
    // directory. Polymarket's egress is DECLARED from that read into the bridge before any client
    // exists (the broker's feed clients, the recorder's rolling families and the venue catalog all
    // dial through it) — a build that links neither Polymarket feature has no client to configure
    // and declares nothing. The broker's client table below takes its copy of the same read
    // (Polymarket's socket batching, each CEX feed's mark-stream row).
    let loaded_venue_settings =
        booted.settings_dir.as_deref().map(vike_secrets::venue_setting::load_venue_settings);
    let venue_settings = venue_settings_of(loaded_venue_settings.as_ref());
    #[cfg(any(feature = "venue-polymarket", feature = "catalog-serve"))]
    declare_polymarket_egress(booted.settings_dir.as_deref(), loaded_venue_settings);

    // ⚠ **THE STOP HANDLERS, AND ONLY WHEN A RECORDING WAS ASKED FOR.** `crate::recorder::arm`'s
    // own doc carries why this is as early as it can be (the subscriber has to exist first, because
    // the outcome is LOGGED) and why it must be before the store opens, the port binds and the
    // first feed thread starts: until it runs, SIGTERM has the OS default disposition and the
    // process dies where it stands with no flush.
    //
    // ⚠ And why it is CONDITIONAL. A serve-only data server has no teardown to run, so installing a
    // handler that raises a flag nothing reads would take SIGTERM's default kill away from it — a
    // daemon that stops answering `systemctl stop` is a worse regression than anything this merge
    // fixes. Without `--record` this line does nothing and the process keeps the behaviour it has
    // always had.
    #[cfg(feature = "record")]
    let stop = record.as_ref().map(|_| crate::recorder::arm());

    // The datahub NODE KEYS (`docs/decisions/0025-datahub-remote-posture.md`), from the credential
    // store — read HERE, off `booted.settings_dir_override`, which is what THIS process's ONE
    // project walk answered. Using the boot's answer rather than a walk of this binary's own is
    // what keeps the log home, the store root and the credentials all pointing at one project;
    // `vike-recorder`'s `webhook_targets` threads the same value into its own store read for
    // exactly that reason, after the same defect bit it.
    //
    // ⚠ `vike_secrets::resolve_project`, not `vike_bridge_core::credentials::…`, and that is a
    // WEIGHT decision: the canonical wrapper drags the ureq/tungstenite/rustls transport stack,
    // which is absent from a DEFAULT vike-datahub graph and must not join it to read a `.env`.
    // vike-secrets names nothing above the vocabulary floor and is already in this crate's
    // closure. (⚠ That read "is a zero-vike-dependency leaf" until
    // `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`, accepted
    // 2026-09-20, admitted `vike-model`. The weight argument is unchanged — the RANK is what it
    // rested on — and that one edge is in this closure already.)
    //
    // ⚠ A store that EXISTS and cannot be READ is NOT "no credentials". The first is a permissions
    // bug that silently drops this server to unauthenticated; the second is the ordinary
    // unconfigured state. They must never look the same to an operator, so the unreadable case is
    // an `error!` naming the consequence, not a shrug.
    // ⚠ WHY THIS FLAG EXISTS: an UNREADABLE store and an ABSENT one both arrive downstream as an
    // empty credential map — deliberately, and documented (`CLAUDE.md`: a store that is present and
    // unopenable logs `error!` and returns an EMPTY map). That is fine while the consequence is
    // "stay unauthenticated"; it stopped being fine when the bind guard turned the same state into
    // a REFUSAL, because the refusal then tells an operator whose keys are merely unreadable that
    // they have "NO node keys" — and `Restart=on-failure` repeats that wrong diagnosis forever.
    // The loader's behaviour is untouched (that is `0013`'s degrade case, and other consumers rely
    // on it); only this binary's ability to SAY WHICH case it is changes.
    let mut store_unreadable = false;
    // ⚠ THE NODE STORE, NOT THE CREDENTIAL STORE — and for this binary the distinction is the whole
    // point. This server needs a node key pair and NOTHING else: it does not trade, holds no venue
    // account and signs no order. Until 2026-09-08 it called `resolve_project`, which opens
    // `<project>/settings/secrets.env` — the file holding every venue key on the box, 168 names of
    // them — to find two. `resolve_node_keys` reads `node.env`, falls back to the old file only
    // while a box has not migrated, and says which it used.
    //
    // A deployment can now give this service a settings directory whose `node.env` holds its two
    // keys and whose `secrets.env` does not exist at all, and it starts correctly authenticated —
    // which is what "compute-to-data" should have meant from the start.
    //
    // ⚠ Decision 0097 added ONE optional read of the CREDENTIAL store, and it is not this one: the
    // OANDA history lane's practice token, one scoped name per Backfill request for oanda
    // (`oanda_history_credentials`). A box without it — or without a credential store at all —
    // starts and serves every verb exactly as before; only a Backfill for oanda is refused.
    //
    // ⚠ The probe is THIS SERVICE'S FAMILY, not all four platform names. `resolve_node_keys`
    // answers WHICH FILE and this root reads its own pair out of it, so the wide predicate let the
    // OTHER service's migration decide this one's: a `node.env` holding only the TRADEHUB pair
    // (what `vike-cli node setup` writes) answered `NodeFile`, and this daemon's datahub pair still
    // in `secrets.env` resolved to nothing — keyless, with no migration notice to explain it.
    let (credentials, key_source): (std::collections::HashMap<String, String>, _) =
        match vike_secrets::resolve_node_keys(
            booted.settings_dir_override.as_deref(),
            vike_model::credential_keys::is_datahub_node_key,
        ) {
            Ok((resolved, source)) => {
                // The store's own findings. `vike-secrets` returns them as DATA (it carries no
                // logging dependency) and `vike_bridge_core::credentials::
                // try_load_workspace_secrets_at` is what normally logs them — this root does not
                // go through that wrapper (see the ⚠ above), so it owes the same surfacing rather
                // than being the one binary where a 0644 credential file is read in silence.
                if let Some(w) = &resolved.warning {
                    tracing::warn!("{w}");
                }
                if let Some(w) = &resolved.legacy {
                    tracing::warn!("{w}");
                }
                (resolved.secrets.into_map(), source)
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "vike-datahub: credential store PRESENT but UNREADABLE — any configured \
                     datahub node keys were NOT loaded. On a LOOPBACK bind this server is about \
                     to serve UNAUTHENTICATED behind the bind guard alone; on a NON-LOOPBACK one \
                     it will now REFUSE TO START a few lines below, because keyless plus \
                     off-box is the combination the guard exists for. Either way the cause is \
                     this file: fix the store's permissions and restart"
                );
                store_unreadable = true;
                (Default::default(), vike_secrets::NodeKeySource::Absent)
            }
        };
    // Said ONCE, at the root that read it, naming the file and the move. A daemon that keeps its
    // node keys in the venue-key file still works and is told, every start, what to do about it.
    if key_source == vike_secrets::NodeKeySource::LegacyCredentialStore {
        // `settings_dir` is an OPTION here — a box with no project above its working directory is a
        // legitimate state — so the notice names the directory the store resolver actually used
        // rather than unwrapping, and falls back to the placeholder the message reads well with.
        let dir = vike_secrets::workspace_node_path_from(booted.settings_dir_override.as_deref())
            .parent()
            .map_or_else(|| "<project>/settings".to_string(), |d| d.display().to_string());
        tracing::warn!("{}", vike_secrets::legacy_node_key_notice(&dir));
    }
    let keys = vike_node_proto::auth::node_keys_from_vars(&credentials);

    // `"market_data/hist"` used to be the last resort here — a CWD-RELATIVE literal, so launching the
    // server from anywhere but the repo root silently created an empty store beside the shell and
    // answered every query with zero rows. Resolved through the shared precedence instead: an
    // explicit `VIKE_DATAHUB_STORE`, then `VIKE_HIST_STORE`, then the repo checkout if this machine
    // has one, then this PROJECT's own `<project>/market_data/hist`, then the per-user `…/vike-data`.
    // The project rung matters most for exactly this binary: it runs from its project root under
    // systemd, where there is no checkout, and before that rung existed the store resolved under
    // the service user's `$HOME` — outside the directory the operator installed and backs up.
    // ⚠ ...and the repo checkout is a DEBUG-BUILD rung only. `env!("CARGO_MANIFEST_DIR")` is a
    // string literal in the binary, and `release.yml` builds the multicall that links it
    // (`vike-backend`; a standalone `vike-datahub` asset too, when this was measured) `--release`
    // on a runner whose checkout path names the runner
    // account — a path `--remap-path-prefix` cannot touch, because that flag rewrites what the
    // COMPILER emits and not what a crate embeds itself. The release's box-path guard
    // (`scripts/refuse_box_paths.sh`) refused both assets by name. A release build passes no rung
    // and resolves from the project walk down, which is the rung this binary takes under systemd
    // anyway; `vike_model::store_path`'s module doc (rung 4) carries the argument, and the
    // attribute rather than `cfg!()` is what keeps the literal out of the bytes.
    #[cfg(debug_assertions)]
    let repo_default = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(|r| r.join("market_data").join("hist"));
    #[cfg(not(debug_assertions))]
    let repo_default: Option<std::path::PathBuf> = None;
    //
    // ⚠ `resolve_store_root_from`, never the bare `resolve_store_root` ladder: that one's project
    // and per-user defaults are two adjacent `Option<PathBuf>`s, so transposing them here would
    // COMPILE SILENTLY and put the tape somewhere else. This form has no two arguments of the same
    // type, and it resolves the project WALK (and `$VIKE_SETTINGS_DIR`, and the
    // `XDG_DATA_HOME`/`HOME`/`LOCALAPPDATA` trio) inside `vike_model` out of the environment map
    // this binary collects — the same shape as the `project_log_dir` call above.

    let resolved = vike_model::store_path::resolve_store_root_from(
        vars.get("VIKE_DATAHUB_STORE").cloned().map(std::path::PathBuf::from),
        vars.get("VIKE_HIST_STORE").cloned(),
        repo_default.as_deref(),
        cwd,
        vars,
    );
    // Say WHICH root, and by which rung, BEFORE anything opens it. A store does not merge: if this
    // answer ever moves, the old store is simply no longer read and every query returns zero rows —
    // which looks exactly like an empty date range. This line is what makes that visible, and it
    // comes before the open so a failure to open is preceded by the path that failed.
    tracing::info!(
        store = %resolved.root.display(),
        rung = resolved.rung.as_str(),
        "hist store root resolved: {}",
        resolved.rung.why()
    );
    let store_dir = resolved.root.display().to_string();

    // ⚠⚠ **EVERY RECORDING REFUSAL HAPPENS HERE — BEFORE THE BIND GUARD, THE STORE OPEN AND THE
    // LISTENER.** `crate::recorder::load_and_check_profile` carries the four questions and the
    // argument; the ORDER is this file's to keep, and getting it wrong has a name. These checks
    // used to live inside `crate::recorder::record`, which this file reaches only after the port is
    // bound and the serve thread is spawned — so a profile with a typo'd `store` key did not
    // REFUSE TO START, it bound `VIKE_DATAHUB_ADDR`, brought the wire up, refused, exited, and let
    // `Restart=on-failure` do it again every five seconds. A crash-loop is strictly worse than a
    // refusal: it takes the data wire up and down, it buries the one line that says why under a
    // repeating boot sequence, and `systemctl status` shows "activating" rather than "failed".
    //
    // Everything below this point can still fail (a port in use, a store that will not open) — but
    // those are failures of an ACTION, and this is a refusal of a CONFIGURATION. Exit 2 is the
    // family this binary already uses for the second kind, and it is deliberately distinct from the
    // `1` a recording that RAN and then broke returns through `finish_recording`.
    #[cfg(feature = "record")]
    let profile = match record.as_ref() {
        Some(req) => {
            // ⚠ FIRST, and before anything that can fail: the retired-flag warning. A deploy that
            // rolls back after this point must still have left the operator the one line telling
            // them to edit the unit — a warning only printed on the happy path is a warning nobody
            // gets on the day it matters. `vike_log::init` has run by here, so it reaches the JSON
            // file layer every service manager captures.
            if let Some(w) = &req.retired_flag_warning {
                tracing::warn!("{w}");
            }
            // ⚠ The ROW arm reads the store BEFORE the bind guard too, which is the whole point of
            // this block's position in the sequence — see `crate::recorder`'s
            // `load_and_check_profile_row` for the ONE question that differs (a profile NAME the
            // store does not hold is a REFUSAL, not the empty answer `read_profiles` gives a
            // selection).
            let loaded = match &req.profile {
                ProfileSource::Row(name) => crate::recorder::load_and_check_profile_row(
                    name,
                    booted.settings_dir_override.as_deref(),
                    &resolved.root,
                    cwd,
                ),
                // The VALUELESS flag. It reads the same store through the same loader; only the
                // NAME is resolved rather than given, and it is resolved by the one function the
                // CLI's own default also calls (`Profiles::resolve_active`), so the profile the
                // CLI calls the default and the profile this daemon mounts cannot become two
                // answers. Its three refusals are three different next commands — see
                // `crate::recorder::load_and_check_active_profile_row`.
                ProfileSource::ActiveRow => crate::recorder::load_and_check_active_profile_row(
                    booted.settings_dir_override.as_deref(),
                    &resolved.root,
                    cwd,
                ),
            };
            match loaded {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "vike-datahub: refusing to start — the recording was refused before \
                         anything was opened or bound, so nothing on this box changed"
                    );
                    eprintln!("vike-datahub: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        None => None,
    };

    let addr = vars
        .get("VIKE_DATAHUB_ADDR")
        .cloned()
        .unwrap_or_else(|| crate::server::DEFAULT_ADDR.to_string());

    // THE ONE OWNER OF VENUE CLIENTS in this process (`crate::feeds`). Built before either plane is
    // used, so both can take their client from it and a key both want is subscribed once. Building
    // it opens nothing: a venue client is constructed on its first subscribe. Its two holders are
    // the md plane (`crate::md::venues::market_builder`) and the recorder
    // (`crate::recording::build_recording_feed`, reached from both `crate::recorder::record` calls
    // below).
    let broker = crate::feeds::FeedBroker::new(
        crate::feeds::venues::real_client_table(venue_settings),
        crate::feeds::venues::sharing_for,
    );

    // ⚠ **THE DRY RUN BINDS NOTHING**, so it is answered here — before the bind guard, before the
    // listener, and before anything can fail on a port. `--once` is a commissioning check an
    // operator runs BY HAND, usually while the daemon it is checking is already running and already
    // holding `VIKE_DATAHUB_ADDR`; a dry run that died on `EADDRINUSE` would be answering a
    // question nobody asked. It still opens the store, because "the store opens" is one of the four
    // things `docs/ops/recorder-deploy.md` says `--once` proves.
    //
    // ⚠ Its EXIT STATUS is the verdict, not its log: `0` only if every feed ended that tick with a
    // live subscription, `4` otherwise. That distinction is why `crate::recorder::EXIT_DRY_RUN`
    // exists at all — see its doc for the the CI box run that exited 0 having resolved nothing.
    // (`profile.clone()` rather than a move: this arm is CONDITIONAL and the daemon arm below needs
    // the same value. It is a handful of paths and subscriptions, cloned once at startup, on the
    // path that exits after one tick.)
    #[cfg(feature = "record")]
    if let (Some(req), Some(profile), Some(stop)) =
        (record.as_ref().filter(|r| r.once), profile.clone(), stop.as_ref())
    {
        let store = match open_hist_store(&store_dir) {
            Ok(s) => Arc::new(s),
            Err(code) => return code,
        };
        return finish_recording(crate::recorder::record(
            &to_record_args(req),
            profile,
            store,
            stop,
            booted.settings_dir_override.as_deref(),
            cwd,
            vars,
            &broker,
        ));
    }

    // Classify the bind target BEFORE the store opens or the listener binds, and REFUSE a
    // non-loopback bind without the named opt-in — the tradehub treatment
    // (`crates/vike-tradehub/src/server.rs`'s `bind_decision`), mirrored into this crate's own
    // `server.rs`. It matters MORE here than there whenever this server is KEY-LESS — which is
    // still the default: that build authenticates NOTHING (the `Hello` informs, it does not gate),
    // so loopback plus the SSH tunnel is the ONLY barrier in front of history reads,
    // and — in a `backfill-serve` build — WRITES into the store plus the irreversible DeleteSeries, and
    // a key-less server is therefore refused a non-loopback bind even WITH the opt-in (see the ⚠
    // below). On a KEYED server the guard still applies, for a different reason: the handshake is
    // plaintext, so the tunnel is what supplies confidentiality and integrity either way. And the
    // shipped unit cannot hold the posture by itself: its
    // `EnvironmentFile=` (the project `.env`) beats its own `Environment=` bind default, so before
    // this guard one `.env` line exposed the server with no unit edit and no warning.
    //
    // The opt-in is the EXACT string "1" (the `VIKE_RECONCILE` master-gate idiom, off the one
    // `vars` sweep this root owns), and it is a SEPARATE knob rather than an inference from the
    // address because the mistake this catches is typing an address — no address can be its own
    // consent. ⚠ A refusal EXITS, where the tradehub daemon keeps trading headless: serving is
    // this process's only job, so "do not bind" and "do not run" are the same decision. Exit 2 —
    // the refused-configuration family this binary already uses for a rejected argument — not
    // FAILURE: nothing was tried and failed; the configuration was refused before anything opened.
    //
    // ⚠ The KEYS are the guard's THIRD input, not a decoration on its message. `ServerAuth::of`
    // classifies the very value handed to `serve_authed` below, so the guard and the server cannot
    // disagree about whether this process authenticates — and a non-loopback bind on a KEY-LESS
    // server is now REFUSED rather than warned about, opt-in or no opt-in. That combination used
    // to be a `ProceedExposed(_) if keys.is_none()` arm right here that logged a `warn!` and FELL
    // THROUGH to bind and serve, which is the state
    // `docs/decisions/0025-datahub-remote-posture.md` names in its own reopening conditions: "an
    // opt-in warning in front of an unauthenticated write verb is the exact state this record
    // exists to prevent". The decision lives in `server.rs` rather than as an `if keys.is_none()`
    // guard here for two reasons: it is CHEAPER to test there, beside the rest of the policy — ⚠ not
    // because a match arm in this `main` is unreachable from a test, which the first draft of this
    // comment claimed and `crates/vike-datahub/tests/help_cli.rs` refutes by spawning this very
    // binary and asserting its status and streams (and which now covers this refusal end to end) —
    // and a NEW ENUM VARIANT makes every match on
    // `BindDecision` — this one and any future caller's — fail to compile until its author
    // confronts the case, where a dropped match guard would silently restore the old behaviour.
    let allow_public = vars.get("VIKE_DATAHUB_ALLOW_PUBLIC_BIND").map(String::as_str) == Some("1");
    let resolved_addrs: Vec<std::net::SocketAddr> =
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    let server_auth = vike_datahub_client::bind::ServerAuth::of(keys.as_ref());
    match vike_datahub_client::bind::bind_decision(&resolved_addrs, allow_public, server_auth) {
        vike_datahub_client::bind::BindDecision::Proceed => {}
        // Reachable only on a KEYED server now — `bind_decision` answers the key-less half with
        // `RefuseUnauthenticated` instead — so this text may say "node keys ARE configured"
        // without a match guard to check it.
        vike_datahub_client::bind::BindDecision::ProceedExposed(exposed) => {
            tracing::warn!(
                %addr, %exposed,
                "vike-datahub binding a NON-LOOPBACK address (VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1). \
                 Node keys ARE configured, so every connection must authenticate — but the \
                 handshake is PLAINTEXT and authenticates the CONNECTION, not each frame, so keep \
                 an SSH tunnel or a VPN in front of it for confidentiality and integrity"
            );
        }
        // ⚠ The combination this guard gained: reachable off-box, consented to, and authenticating
        // NOTHING. The message names both variables and both fixes — and names no VALUE: `keys` is
        // a redacting-`Debug` `NodeKeys` and nothing here reads a credential, so no secret can
        // reach a log line from this arm.
        vike_datahub_client::bind::BindDecision::RefuseUnauthenticated(exposed)
            if store_unreadable =>
        {
            // ⚠ THE SAME REFUSAL, THE OTHER CAUSE. Reaching the arm below instead would tell an
            // operator who HAS keys that they have none, and `Restart=on-failure` would repeat it
            // until somebody read the earlier `error!` line and connected the two. The disposition
            // is unchanged — still fail closed, still exit 2 — only the diagnosis is true.
            tracing::error!(
                %addr, %exposed,
                "VIKE_DATAHUB_ADDR is NOT loopback and this server could not READ its credential \
                 store, so it has no node keys to authenticate with — refusing to start. This is \
                 NOT a missing configuration: the store is present and its keys may well be \
                 correct. Fix the file's permissions (`vike-cli secrets path` prints it; the \
                 earlier `credential store PRESENT but UNREADABLE` line names the OS error) and \
                 restart. Until then this daemon will keep exiting 2 and being restarted"
            );
            return ExitCode::from(2);
        }
        vike_datahub_client::bind::BindDecision::RefuseUnauthenticated(exposed) => {
            tracing::error!(
                %addr, %exposed,
                "VIKE_DATAHUB_ADDR is NOT loopback and this server has NO node keys — refusing to \
                 start. VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 consents to being REACHABLE; it is not \
                 consent to serve history, and (in a backfill-serve build) WRITES into the store \
                 plus the IRREVERSIBLE DeleteSeries, to anyone who can open a socket. \
                 Either set VIKE_DATAHUB_OBSERVE_KEY (reads) and VIKE_DATAHUB_CONTROL_KEY (the \
                 Backfill write and the DeleteSeries removal) in the credential store — \
                 `vike-cli secrets path` prints the file — or put the address back on 127.0.0.1 \
                 and reach it with `ssh -L 7878:localhost:7878 <host>`. Keep the tunnel or a VPN \
                 either way: the handshake is plaintext even when the keys ARE set"
            );
            return ExitCode::from(2);
        }
        vike_datahub_client::bind::BindDecision::Refuse(exposed) => {
            tracing::error!(
                %addr, %exposed,
                keyed = keys.is_some(),
                "VIKE_DATAHUB_ADDR is NOT loopback — refusing to start. This server is meant to be \
                 reached over an SSH tunnel (its handshake is plaintext even when node keys ARE \
                 set): keep the address on 127.0.0.1 and run `ssh -L 7878:localhost:7878 <host>`. \
                 If this box genuinely must listen on a trusted network, set \
                 VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 — and set VIKE_DATAHUB_OBSERVE_KEY / \
                 VIKE_DATAHUB_CONTROL_KEY in the credential store first, so what is exposed is \
                 authenticated"
            );
            return ExitCode::from(2);
        }
    }

    let store = match open_hist_store(&store_dir) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(addr = %addr, error = %e, "vike-datahub: failed to bind listener");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(addr = %addr, store = %store_dir, "vike-datahub: listening");

    let store = Arc::new(store);
    // ⚠ **THE LIVE MARKET-DATA PLANE, ARMED BY AN OPERATOR AND NOT BY A BUILD.**
    //
    // `VIKE_DATAHUB_LIVE` is read as the EXACT string "1" — this workspace's idiom for a switch
    // whose safe state is off — out of the ONE `std::env::vars()` sweep the bin already owns, so
    // both rows stay `Layer::Injected`/`Naming::MapLookup` in `vike_ops::settings::SETTINGS` and
    // `LIBRARY_PIN` does not grow (`library_rows_do_not_grow` refuses an addition).
    //
    // ⚠ The `.get(` calls below are DELIBERATELY literal rather than routed through a local
    // closure: `crates/vike-ops/tests/settings_registry.rs`'s `MAP_LOOKUP_PROVEN` is a SHAPE-keyed
    // measurement of which `MapLookup` rows the scanner can positively prove at a real `.get(`
    // site, pinned in BOTH directions. A `let get = |k| vars.get(k)` helper would drop both keys
    // into the measured blind spot and redden that pin from the removal direction.
    //
    // ⚠ Unset is BYTE-IDENTICAL to a build without the plane: no hub is mounted, `served_features`
    // advertises neither `market_data` nor any `md_venue=` entry, and `MdSubscribe` is answered
    // with `crate::server::NO_MARKET_DATA_PLANE` on a connection that stays positional. That is the
    // credential-is-the-gate idiom pointed at a CAPABILITY: the build carries it, the operator arms
    // it, and the two cannot answer differently because the advertisement keys on the MOUNT.
    let md_hub = if vars.get("VIKE_DATAHUB_LIVE").map(String::as_str) == Some("1") {
        // ⚠ NO `#[cfg]` HERE, and that is the whole point of the feature-free hub. The venue TABLE
        // is feature-free code with cfg'd ARMS, so a build with no venue feature mounts a hub that
        // refuses every venue BY NAME — naming the feature to rebuild with — rather than serving
        // nothing quietly. That is a real configuration, exactly as `record` alone is, and it is
        // `crate::recording::build_recording_feed`'s rule one level up.
        let hub = crate::md::MdHub::new(
            crate::md::venues::market_builder(Arc::clone(&broker)),
            crate::md::venues::supported().into_iter().map(str::to_string).collect(),
        );
        // Tier R — the RESIDENT set. Parsed from the same sweep; an unparseable row is a WARNING
        // that names the row rather than a startup refusal, because a typo in one resident key must
        // not take the whole data wire down (`docs/decisions/0013-degrade-vs-refuse.md`).
        let resident_raw = vars.get("VIKE_DATAHUB_LIVE_RESIDENT").cloned().unwrap_or_default();
        let (resident, bad) = crate::md::parse_resident_set(&resident_raw);
        for row in &bad {
            tracing::warn!(
                row = %row,
                "vike-datahub md: ignoring an unparseable VIKE_DATAHUB_LIVE_RESIDENT row — the \
                 format is venue:symbol:lane with lane one of depth|book|trades"
            );
        }
        // ⚠ **A ROW THAT PARSES IS NOT A ROW THAT CAN BE SERVED**, and `parse_resident_set` checks
        // only the three-field shape and the lane word — `notavenue:X:depth` and a real venue on a
        // lane its declared caps do not serve both get through it. `add_resident` runs `acquire`'s
        // capability checks and the two key caps, and each refusal is named HERE for the same
        // reason the unparseable rows are: a resident key the daemon silently could not subscribe
        // is an endless 5-second reconcile-failure loop with no line saying which row caused it.
        let mut pinned = 0usize;
        for spec in &resident {
            match hub.add_resident(spec) {
                Ok(()) => pinned += 1,
                Err(why) => tracing::warn!(
                    venue = %spec.venue,
                    symbol = %spec.symbol,
                    lane = ?spec.lane,
                    %why,
                    "vike-datahub md: REFUSING a VIKE_DATAHUB_LIVE_RESIDENT row — it is not pinned \
                     and nothing will retry it. Every other row is unaffected"
                ),
            }
        }
        hub.spawn();
        tracing::info!(
            venues = ?hub.served_venues(),
            resident = pinned,
            "vike-datahub: LIVE MARKET-DATA plane armed (VIKE_DATAHUB_LIVE=1) — this process is now \
             the single subscriber to each venue it serves, and spends that venue's API budget from \
             this box's IP"
        );
        Some(hub)
    } else {
        None
    };

    // Backfill-on-demand (split-plane REQ-9): a `backfill-serve` build mounts the REAL collector
    // table over the SAME store handle the server serves, which is what makes `serve` advertise
    // and answer `Request::Backfill`; any other build mounts none and the verb is a clean refusal.
    //
    // ⚠ Its CREDENTIALED row (OANDA's, decision 0097) takes a token PROVIDER built here,
    // off this boot's one settings directory — the same directory the Polymarket egress read above
    // uses — and the provider holds that directory and nothing else. The ONE read this block makes
    // itself is the startup line's: it is the read that logs the store's permission finding (the
    // provider's own per-request reads are silent), and its token is dropped inside the `map` before
    // anything is logged — the line reports PRESENCE, never a value.
    #[cfg(feature = "backfill-serve")]
    let backfill = {
        let presence = read_oanda_history_token(booted.settings_dir.as_deref(), true).map(drop);
        let oanda_history_token = oanda_history_token_reader(booted.settings_dir.clone());
        let line = oanda_history_lane_line(booted.settings_dir.as_deref(), presence);
        match presence {
            Err(vike_oanda::HistoryTokenError::StoreUnreadable) => {
                tracing::warn!("vike-datahub: {line}")
            }
            Ok(()) | Err(vike_oanda::HistoryTokenError::NotConfigured) => {
                tracing::info!("vike-datahub: {line}")
            }
        }
        Some(crate::backfill::real_backfill_table(Arc::clone(&store), oanda_history_token))
    };
    #[cfg(not(feature = "backfill-serve"))]
    let backfill: Option<crate::backfill::BackfillTable> = None;

    // ⚠ **THE CHART-GAP SEED LANE, ARMED BY AN OPERATOR AND NOT BY A BUILD** — the same idiom as
    // `VIKE_DATAHUB_LIVE` above, the EXACT string "1", read out of the ONE `std::env::vars()` sweep
    // this bin already owns so the row stays `Layer::Injected`/`Naming::MapLookup` in
    // `vike_ops::settings::SETTINGS` and `LIBRARY_PIN` does not grow. The `.get(` is DELIBERATELY
    // literal for `MAP_LOOKUP_PROVEN`'s sake, for the reason spelled at the market-data arm above.
    //
    // ⚠ The lane's mere EXISTENCE is the arming: `crate::server`'s `served_features` keys
    // `FEATURE_SEED_SERIES` on `Option::is_some`, so there is no second switch for the
    // advertisement to disagree with. And an UNARMED daemon still answers the verb — successfully,
    // having written nothing — which is the leg that makes a WRITE verb's `VerbScope::Read`
    // classification honest. `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` puts
    // removing this switch, or defaulting it ON, in its reopen list.
    let seed_lane = if vars.get("VIKE_DATAHUB_CHART_SEED").map(String::as_str) == Some("1") {
        if backfill.is_none() {
            // Armed on a build with no collectors: the lane would advertise a capability it can
            // only refuse. Say so ONCE at startup rather than once per request, and arm nothing —
            // `docs/decisions/0013-degrade-vs-refuse.md`: a capability degrades, it does not refuse
            // to start.
            tracing::warn!(
                "vike-datahub: VIKE_DATAHUB_CHART_SEED=1 but this build carries no \
                 collector table — the chart-seed lane is NOT armed and `seed_series` \
                 is not advertised. Rebuild with `--features backfill-serve`"
            );
            None
        } else {
            tracing::info!(
                "vike-datahub: CHART-GAP SEED lane armed (VIKE_DATAHUB_CHART_SEED=1) — an \
                 OBSERVE-scope client may now have this process fetch one bounded kline \
                 window per series into its store, spending this box's venue-API budget. \
                 Every bound is this server's; see crates/vike-datahub/src/seed.rs"
            );
            Some(std::sync::Arc::new(crate::seed::SeedLane::new()))
        }
    } else {
        None
    };

    // ⚠ **THE VENUE-CATALOG LANE, ARMED BY AN OPERATOR AND NOT BY A BUILD** — the same idiom as
    // the two arms above, the EXACT string "1", read out of the ONE `std::env::vars()` sweep this
    // bin already owns so the row stays `Layer::Injected`/`Naming::MapLookup` in
    // `vike_ops::settings::SETTINGS` and `LIBRARY_PIN` does not grow. The `.get(` is DELIBERATELY
    // literal for `MAP_LOOKUP_PROVEN`'s sake, for the reason spelled at the market-data arm above.
    //
    // ⚠ **A build feature could NOT be the arming**, and that is argued rather than assumed:
    // `docs/decisions/0035-the-image-ships-every-feature-and-may-be-the-primary-install.md` means a
    // `catalog-serve`-gated default would be ON wherever the image is the install, which is exactly
    // where it matters most. The switch has to be runtime.
    //
    // ⚠ Unlike the seed lane above, an armed lane on a table-less build is NOT refused here. It is
    // armed with an EMPTY table, and every venue then answers a `NotServed` naming an empty
    // supported set. The difference is deliberate and is the honest direction: the seed lane's
    // unarmed answer is indistinguishable from a working lane that found no rows, so arming it
    // without collectors would mislead — while THIS verb's `NotServed` says outright that the build
    // carries no providers, which is precisely what an operator who set the variable needs to read.
    // `docs/decisions/0013-degrade-vs-refuse.md`: a capability degrades, it does not refuse to
    // start.
    //
    // ⚠ **THE DEFAULT FLIPPED on 2026-09-16 and this block used to read the opposite way.** It was
    // `vars.get("VIKE_DATAHUB_VENUE_CATALOG") == Some("1")`, default OFF, and
    // `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`
    // turned it round: without an instrument list you cannot pick a symbol, so a switch that is off
    // by default means the product does not work out of the box — and on the primary install the
    // operator never sees the variable's name at all, because every surface that would have taught
    // it is a surface the switch turns off.
    //
    // The REFUSAL is what survives, as a settings key rather than a variable
    // (`vike_config::Flags::venue_catalog_off`), because
    // `crates/vike-config/tests/flag_registry.rs`'s `every_flag_defaults_off` makes a default-ON
    // positive flag unrepresentable in that type. The verdict is
    // `vike_catalog::venue_catalog_gate`, which lives one crate below every consumer so no second
    // root can word it differently — the `reconcile_gate` shape, copied deliberately.
    //
    // ⚠ The PROVIDER-LESS build still SERVES, exactly as an armed table-less build did before, and
    // the argument is unchanged: this verb's `NotServed` says outright that the build carries no
    // providers, which is what an operator needs to read. `docs/decisions/0013-degrade-vs-refuse.md`
    // — a capability degrades, it does not refuse to start. What is NEW is that the same shape is
    // now reachable without anybody having asked for the lane, which is why the gate gives it its
    // own verdict arm rather than folding it into a warning beside an `Armed` one.
    // `flags.hyperliquid_hip3` — the same resolved row the trading daemon's hyperliquid mount folds
    // (decision 0095), so this catalog's HIP-3 universe cannot disagree with the mount's symbology.
    #[cfg(feature = "catalog-serve")]
    let catalog_table = crate::catalog::real_catalog_table(booted.settings.flags.hyperliquid_hip3);
    #[cfg(not(feature = "catalog-serve"))]
    let catalog_table = crate::catalog::CatalogTable::new(Vec::new());
    // ⚠ This is the site `crates/vike-config/src/consumed.rs`'s row for `flags.venue_catalog_off`
    // names, needle and all, and it must stay an ARGUMENT to the gate rather than a local bound
    // first: a `Consumption` row proves a key is READ, and a value assigned and never passed is
    // exactly the declared-but-unconsumed shape that table exists to catch.
    let catalog_gate = vike_catalog::venue_catalog_gate(
        booted.settings.flags.venue_catalog_off,
        catalog_table.supported().len(),
    );
    {
        // Both answers are news — the gate's module doc, property 3. The REFUSAL is `warn!`
        // because the operator of a box whose symbol picker will be empty has to be able to find
        // out why from the log alone; the served verdict is `info!` because it is the ordinary
        // state.
        let line = vike_catalog::venue_catalog_gate_line(
            catalog_gate,
            &catalog_table.supported().join(", "),
        );
        match catalog_gate {
            vike_catalog::VenueCatalogGate::Armed => tracing::info!("vike-datahub: {line}"),
            _ => tracing::warn!("vike-datahub: {line}"),
        }
    }
    let catalog_lane = catalog_gate
        .serves()
        .then(|| std::sync::Arc::new(crate::catalog::CatalogLane::new(catalog_table)));

    // ⚠ **THE MERGED PROCESS: RECORD ON THE MAIN THREAD, SERVE ON A SPAWNED ONE** (ruling 10). The
    // order matters and it is not arbitrary — see `crate::recorder`'s module doc. The serve loop has
    // no teardown at ALL (`listener.incoming()` for the process lifetime; SIGKILL has always been
    // its stop), while the recorder's teardown is where the buffered tape is either flushed or
    // lost. So the flush keeps the main thread, and abandoning the serve thread when `main` returns
    // is byte-identical to what a `systemctl stop` did to this daemon before the merge.
    //
    // The feeds could not take the spawned thread anyway: `RecorderRuntime` owns
    // `Box<dyn VenueFeed>`, which is not `Send` (`crate::recorder::FEED_STOP_BUDGET_SECS`' doc
    // carries what that costs the teardown), so it is built and dropped where it is used.
    #[cfg(feature = "record")]
    if let (Some(req), Some(profile), Some(stop)) = (record.as_ref(), profile, stop.as_ref()) {
        let serve_store = Arc::clone(&store) as Arc<dyn vike_data::HistStore + Send + Sync>;
        let serve_stop = stop.flag();
        // Both planes hold clients from ONE broker (docs/decisions/0092), so a shared key is
        // subscribed once.
        let serve_md = md_hub.clone();
        let serve_seed = seed_lane.clone();
        let serve_catalog = catalog_lane.clone();
        std::thread::spawn(move || {
            match crate::server::serve_authed(
                listener,
                serve_store,
                backfill,
                keys,
                serve_md,
                serve_seed,
                serve_catalog,
            ) {
                // ⚠ NEITHER ARM IS REACHABLE IN PRACTICE, and the reaction is the same for both:
                // raise the shared stop flag. `serve_authed` loops over `listener.incoming()`,
                // which does not end for a TCP listener, and logs-and-continues on a failed accept
                // — so arriving here at all means the socket is gone. Recording on with a dead wire
                // would leave a daemon that looks healthy and answers nothing, and simply exiting
                // the thread would do exactly that. Raising the flag runs the RECORDER'S teardown
                // first (the rows are flushed) and then lets `Restart=on-failure` rebuild both
                // halves, which is the only ordering that loses nothing.
                Ok(()) => tracing::error!(
                    "vike-datahub: the serve loop ENDED — stopping the recorder gracefully so the \
                     buffered tape is flushed before this process exits and is restarted"
                ),
                Err(e) => tracing::error!(
                    error = %e,
                    "vike-datahub: serve loop ended with an error — stopping the recorder \
                     gracefully so the buffered tape is flushed before this process exits and is \
                     restarted"
                ),
            }
            vike_ops::stop::request_stop(&serve_stop);
        });
        return finish_recording(crate::recorder::record(
            &to_record_args(req),
            profile,
            store,
            stop,
            booted.settings_dir_override.as_deref(),
            cwd,
            vars,
            &broker,
        ));
    }

    match crate::server::serve_authed(
        listener,
        store,
        backfill,
        keys,
        md_hub,
        seed_lane,
        catalog_lane,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "vike-datahub: serve loop ended with an error");
            ExitCode::FAILURE
        }
    }
}

/// Open the ONE hist store this process serves — and, under `--record`, writes.
///
/// Extracted because the dry run opens it too, from a different point in the sequence (before the
/// bind guard, since it binds nothing), and two `DataFusionHist::open` call sites with two error
/// messages is how a store failure starts reading differently depending on which flag you passed.
#[cfg(feature = "serve-datafusion")]
fn open_hist_store(store_dir: &str) -> Result<vike_data::DataFusionHist, ExitCode> {
    vike_data::DataFusionHist::open(store_dir).map_err(|e| {
        tracing::error!(store = %store_dir, error = %e, "vike-datahub: failed to open hist store");
        ExitCode::FAILURE
    })
}

/// The recording's own exit statuses, kept in ONE place so the dry run and the daemon cannot map
/// the same outcome differently.
///
/// `EXIT_SILENT` (3) and `EXIT_DRY_RUN` (4) are deliberately distinct from `1` (a startup failure)
/// and `2` (a refused configuration): a supervisor should be able to tell "the daemon worked and
/// the DATA did not" apart from "the profile was bad". Their own docs carry the the CI box runs that
/// made each one necessary.
///
/// ⚠ **A bad PROFILE no longer reaches this function at all** — a read failure, a parse failure, a
/// store-root disagreement and a venue this build cannot record are all answered by
/// `crate::recorder::load_and_check_profile` before the listener binds, and exit **2**. What is
/// left for the `Err` arm here is a recording that STARTED and then broke: the writer thread would
/// not spawn, a feed refused to mount. That split is what makes the status readable — `2` means
/// nothing was opened or bound and the fix is in a file; `1` means the daemon ran.
#[cfg(feature = "record")]
fn finish_recording(
    outcome: Result<crate::recorder::RecordOutcome, String>,
) -> std::process::ExitCode {
    match outcome {
        Ok(crate::recorder::RecordOutcome::Stopped) => ExitCode::SUCCESS,
        Ok(crate::recorder::RecordOutcome::Silent(series)) => {
            tracing::error!(
                series = ?series,
                "vike-datahub: exiting on silence (--exit-on-silence) — these subscribed series \
                 are receiving no rows"
            );
            eprintln!("vike-datahub: exiting on silence: {}", series.join(", "));
            ExitCode::from(crate::recorder::EXIT_SILENT)
        }
        Ok(crate::recorder::RecordOutcome::DryRunFailed(reasons)) => {
            for why in &reasons {
                tracing::error!(reason = %why, "vike-datahub: --once dry run FAILED");
            }
            eprintln!(
                "vike-datahub: --once proved NOTHING — this profile would record no data:\n  {}",
                reasons.join("\n  ")
            );
            ExitCode::from(crate::recorder::EXIT_DRY_RUN)
        }
        Err(e) => {
            tracing::error!(error = %e, "vike-datahub: recording failed");
            eprintln!("vike-datahub: {e}");
            ExitCode::FAILURE
        }
    }
}

/// **WHICH MODE IS THIS DAEMON IN** — recording, or serving only — as one line, said at startup
/// before anything opens a store or binds a port.
///
/// ⚠ **It exists because forgetting `--record` is a SILENT no-op, and one shipped.** The runbook's
/// install path put a bare `datahub` on the unit's `ExecStart=`, so an install its own page called
/// "recording" came up healthy, answered every query, passed `systemctl is-active`, and
/// accumulated nothing — indistinguishable from a venue that is merely quiet, and invisible until
/// somebody queried a date range and got zero rows. Every other symptom this daemon has is loud;
/// this one had no symptom at all, so the cure is a line that makes the mode a FACT in the journal:
/// `journalctl -u vike-datahub | grep 'vike-datahub: '` answers it without reading a unit file.
///
/// It is a pure function of the parse so it can be tested in BOTH builds ([`the_mode_line_says_which_mode`]);
/// the `run` bodies below only print it. It names the profile PATH deliberately — on a box with
/// two datahubs "recording" is not enough to tell you WHICH tape.
// Only the serving `run` prints it — the feature-off stub has no subscriber and no run to describe,
// it just says what it cannot do and exits. The tests below exercise it in every build, which is
// what makes this an `allow` rather than a `#[cfg]` on the function itself.
#[cfg_attr(not(feature = "serve-datafusion"), allow(dead_code))]
fn startup_mode_line(record: Option<&RecordRequest>) -> String {
    match record {
        // ⚠ The SOURCE is named, not just the profile: since 2026-09-16 a profile can come from a
        // file or from a `recorder` row, and "which store answered" is exactly the question an
        // operator reading a journal hours later cannot reconstruct. It is the same reason
        // `vike-cli secrets list` leads with a `source:` line.
        Some(req) => format!(
            "vike-datahub: RECORDING AND SERVING — venue feeds fill the very store this server \
             answers from ({})",
            match &req.profile {
                ProfileSource::Row(n) =>
                    format!("{RECORDER_PROFILE_FLAG} {n} (a row in the settings store)"),
                // ⚠ It names the FLAG and not a profile, because at this point in the startup
                // sequence no store has been read and the name is genuinely not known yet. The
                // resolved name reaches the journal through `RecordArgs::profile`, which is built
                // after the load — a line here that guessed one would be the wrong kind of
                // certain.
                ProfileSource::ActiveRow => format!(
                    "{RECORDER_PROFILE_FLAG} with no value — the ACTIVE recorder row in the \
                     settings store"
                ),
            }
        ),
        None => {
            format!(
                "vike-datahub: SERVE-ONLY — no --record or {RECORDER_PROFILE_FLAG}, so this \
                 daemon subscribes to NO venue and writes NO rows; it answers from a store some \
                 other process fills. Add `{RECORDER_PROFILE_FLAG} <name>` to the unit's \
                 ExecStart= to make it record"
            )
        }
    }
}

/// The feature-free [`RecordRequest`] this file parses, as the `record` module's own argument type.
///
/// Two types rather than one because the PARSE must compile in every build and `RecordArgs` lives
/// behind the feature — see [`RecordRequest`]'s doc. The conversion is the seam, and
/// [`the_parser_defaults_match_the_recorders`] is what keeps the two spellings of each default from
/// drifting.
#[cfg(feature = "record")]
fn to_record_args(req: &RecordRequest) -> crate::recorder::RecordArgs {
    crate::recorder::RecordArgs {
        profile: match &req.profile {
            ProfileSource::Row(n) => format!("profile row `{n}`"),
            // ⚠ It says HOW the row was chosen, not just which — "the active row" is the fact an
            // operator needs when a unit's ExecStart= carries no name to compare against. The
            // resolved name is not threaded here deliberately: this struct is built from the
            // PARSE, and reaching into the loaded profile for it would make provenance depend on a
            // read that has already happened somewhere else.
            ProfileSource::ActiveRow => "the ACTIVE recorder profile row".to_string(),
        },
        tick: std::time::Duration::from_secs(req.tick_secs),
        once: req.once,
        silent_secs: req.silent_secs,
        exit_on_silence: req.exit_on_silence,
    }
}

/// Inert stub for a default (feature-off) build: there is no DataFusion backend to serve, so print
/// a rebuild hint and exit non-zero. Keeps `cargo build -p vike-datahub` producing a runnable bin.
///
/// ⚠ The missing-feature message is a RUN failure, and only that. `--help` and `--version` are
/// answered first, exactly as under the feature: a build that cannot serve can still describe its
/// own command line, and answering `vike-datahub --help` with "rebuild with --features …" told a
/// caller the wrong thing about the wrong question.
#[cfg(not(feature = "serve-datafusion"))]
// ⚠ The two parameters are UNUSED in this arm and keep their names underscored rather than being
// dropped: the signature must match the `serve-datafusion` `run` exactly, or the dispatcher and
// the bin would fail to compile in whichever configuration they were not written against.
pub fn run(
    _vars: &std::collections::HashMap<String, String>,
    _cwd: Option<&std::path::Path>,
    args: &[String],
) -> std::process::ExitCode {
    // The parse runs in BOTH builds and produces the same answers — that is `short_circuit`'s whole
    // job. A `--record` this build cannot honour is therefore a RUN failure below, never a rejected
    // flag: the flag is real and understood, and only the backend is missing.
    if let Err(code) = short_circuit(args) {
        return code;
    }
    eprintln!(
        "vike-datahub was built without the `serve-datafusion` feature, so it has no data backend \
         to serve. Rebuild with: cargo run -p vike-datahub --features serve-datafusion"
    );
    ExitCode::from(2)
}

#[path = "datahub_cli_tests.rs"]
#[cfg(test)]
mod datahub_cli_tests;
