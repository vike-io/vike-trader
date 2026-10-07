//! The command line: the usage text, the parse into a run or a recording request, and `--help`.

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

credential store (the settings database; on a box that has not migrated, node.env and
secrets.env under <project>/settings — `vike-cli secrets path` prints the store):
  VIKE_DATAHUB_OBSERVE_KEY   read scope: history + catalog. Absent = no authentication at all.
  VIKE_DATAHUB_CONTROL_KEY   write scope: the above, plus the Backfill and ImportArchive store
                             writes, the DeleteSeries store REMOVAL. (The Run* verbs, which COMPILE
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
pub(super) struct RecordRequest {
    pub(super) profile: ProfileSource,
    pub(super) tick_secs: u64,
    pub(super) silent_secs: u64,
    pub(super) exit_on_silence: bool,
    pub(super) once: bool,
    /// Set when the RETIRED flag spelling was used — logged once at startup, never silent.
    ///
    /// ⚠ It rides the parse rather than being printed inside it, because `parse_args` runs before
    /// `vike_log::init` and a `println!` there would land outside the JSON file layer every service
    /// manager captures. The same shape the credential store's own findings take a few hundred
    /// lines down: returned as DATA, logged by the caller once a subscriber exists.
    pub(super) retired_flag_warning: Option<String>,
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
pub(super) enum ProfileSource {
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
pub(super) const DEFAULT_TICK_SECS: u64 = 30;

/// The silence-watchdog grace when no `--silent-secs` is given. Same duplication, same gate.
pub(super) const DEFAULT_SILENT_SECS: u64 = 300;

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
/// `crates/vike-ops/tests/deploy/deploy_layout_gate/unit_fixtures.rs`'s `records` decides whether a shipped unit WRITES
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
pub(super) const RECORDER_PROFILE_FLAG: &str = "--recorder-profile";

/// What a successful parse produced. Named variants rather than a bool pair, the shape
/// `vike-tradehub`'s `Parsed` uses — nothing here can be confused for a run.
///
/// `Debug` so a parse that was supposed to FAIL can report what it actually produced instead
/// (`Result::expect_err` requires it) — the whole value of the unknown-argument test below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Parsed {
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
pub(super) fn parse_args(argv: impl Iterator<Item = String>) -> Result<Parsed, String> {
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
pub(super) fn short_circuit(args: &[String]) -> Result<Option<RecordRequest>, ExitCode> {
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
