//! The command line: the `clap` struct the argv is read into, the parse into a [`Launch`] (serve,
//! and record if a profile was named), and `--help` (decision 0155).
//!
//! `clap` reads the argv; `vike_util::cli::Program` decides what `clap` leaves to the program (the
//! one error path and exit code 2, `-h`/`--help` and `-V`/`--version` to stdout and exit 0, a blank
//! value refused, a typo refused with a suggestion, a repeated flag refused). What stays HERE, after
//! the parse, is every cross-flag rule: two profile sources, a companion flag without a profile,
//! `--tick-secs > 0`, `--exit-on-silence` with detection off.

use std::path::PathBuf;

use vike_util::cli::{CliExit, Program, USAGE_EXIT};

/// The name every message of this program carries: `vike-datahub: <message>`, and the `Usage:` line.
const BIN: &str = "vike-datahub";

/// What `--help` prints after the flags: the settings rows and the credential store this daemon
/// reads, none of which is on the command line.
const SETTINGS_HELP: &str = "\
settings (rows of the settings database: `vike-cli config set <key> <value>`, then restart; no
environment variable reaches any of them, and a retired one refuses startup naming its row):
  config.datahub_bind_addr
                       listen address (default 127.0.0.1:7878). A non-loopback address is
                       refused at startup unless flags.datahub_allow_public_bind is also on
                       AND a node key is configured (see the credential store below).
                       The handshake is plaintext even when keys are set, so it is meant to be
                       reached over an SSH tunnel (ssh -L 7878:localhost:7878 <box>).
  flags.datahub_allow_public_bind
                       the explicit opt-in for a non-loopback bind. It consents to being
                       REACHABLE, and is not sufficient on its own: with no node key configured
                       the bind is still REFUSED, because the opt-in is not consent to serve a
                       store write and a store REMOVAL unauthenticated. With a key it proceeds
                       behind a logged warning naming what is exposed.

credential store (the settings database, <project>/settings/db/vike.db — the only store;
`vike-cli secrets path` prints it):
  VIKE_DATAHUB_OBSERVE_KEY   read scope: history + catalog. Absent = no authentication at all.
  VIKE_DATAHUB_CONTROL_KEY   write scope: the above, plus the Backfill and ImportArchive store
                             writes, the DeleteSeries store REMOVAL. (The Run* verbs, which COMPILE
                             CLIENT-SUPPLIED RHAI, moved to `vike-backend backtest --addr`.)
                       With NEITHER set this server authenticates nothing and serves every verb
                       EXCEPT DeleteSeries to whoever can reach the socket — which is why that
                       build is CONFINED to a loopback address (see config.datahub_bind_addr).
                       A key-less server neither advertises the delete verb nor answers one:
                       with nothing authenticating, its Control scope would be a word nothing
                       enforces, and unlike a backfill (whose rows a re-fetch restores) a
                       removal takes the only copy. Delete on the box instead, with
                       `vike-cli data hist rm --store DIR`.
                       Setting either key makes the handshake mandatory on every connection, and
                       is what makes the delete verb exist at all; reaching it additionally needs
                       the CONTROL key, since a server holding none refuses that scope outright.
  oanda API_KEY        the practice account's OANDA API token (the `API_KEY` field of the active
                       unlabelled oanda account at tier `demo`) — the ONE venue credential this
                       server reads (docs/decisions/0097), and only in a `backfill-serve` build.
                       A Backfill for oanda (Control scope) reads it when the request arrives and
                       drops it when the request ends, so storing, rotating or removing it needs
                       no restart; absent, that Backfill is refused naming the fix.
                       No Observe verb reaches it, and a live-account key is never read. The
                       startup log says whether it is present, never its value.

more settings rows:
  config.store_root    hist-store root; unset, falls through a repo checkout if this machine has
                       one, this project's own market_data/hist, then a per-user dir. ⚠ Write it
                       on a box that already records: without it the daemon opens the project
                       default, not the store it has been filling.
  flags.datahub_live   true ARMS the LIVE MARKET-DATA plane: this daemon becomes
                       the single subscriber to each venue's book/depth/tape and pushes them to
                       clients over the MdSubscribe stream. false (the default) = off, and off is
                       byte-identical to a build without the plane — no venue socket, no
                       `market_data` capability in Welcome.features, MdSubscribe refused by name.
                       ⚠ It spends VENUE-API budget from THIS BOX'S IP, shared with the
                       order-signing daemon, which is why it is an explicit operator act and why
                       the per-venue key cap and the 60 s linger exist.
                       Needs a build carrying `--features live-feeds` plus the `live-<venue>`
                       features for the venues you want; a build without them arms a hub that
                       refuses every venue BY NAME rather than serving nothing quietly.
  config.datahub_live_resident
                       comma-separated `venue:symbol:lane` entries PINNED at startup (lane is one of
                       depth|book|trades), e.g. `binance:BTCUSDT.P:depth,binance:BTCUSDT.P:trades`.
                       A resident key has a refcount FLOOR of 1 and is never released, so a hot
                       symbol's ladder paints on the first DOM open instead of waiting on a REST
                       re-seed, and the daemon's traded pair stays observable with no desktop
                       attached. No row = an EMPTY resident set, which is a real configuration:
                       the hub still serves on-demand keys.
  flags.datahub_chart_seed
                       true ARMS the CHART-GAP SEED lane: a client whose chart has
                       no bars for a (venue, symbol, interval) may ask this daemon to fetch ONE
                       bounded window of klines into its own store. false = off, and off is
                       byte-identical to a build without the lane except in one respect that is
                       deliberate: the verb is still ANSWERED, successfully, having fetched
                       nothing. (The `seed_series` capability is absent from Welcome.features
                       either way, so a client that reads the handshake names this row to its
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
  (the VENUE-CATALOG lane is ON BY DEFAULT)
                       A client may ask this daemon for ONE venue's PUBLIC instrument list, and
                       since docs/decisions/0066 that is the DEFAULT: without an instrument list
                       you cannot pick a symbol, so a lane that was off by default meant the
                       product did not work out of the box.
                       ⚠ THE SWITCH IS ITS REFUSAL, the flags.venue_catalog_off row:
                         vike-cli config set flags.venue_catalog_off true
                       Refused, the verb
                       is still ANSWERED, successfully, having fetched nothing, and the
                       `venue_catalog` capability is absent from Welcome.features — so a client
                       that reads the handshake names the refusal to its operator instead of
                       showing an EMPTY symbol list, which for `ig` and `ibkr` would be
                       indistinguishable from the truth.
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
  flags.journal_materializer
                       true RUNS the JOURNAL MATERIALIZER: an off-path thread follows the trade
                       daemon's command journal in the config.journal_dir directory and folds its
                       fills and order lifecycle into this store's exec_fill / exec_order series.
                       false (the default) = nothing runs. On with no config.journal_dir row is a
                       startup warning, and nothing runs.
                       ⚠ Its checkpoint, materializer.ckpt, lives IN the journal directory and is
                       also the journal prune's retention floor: this daemon needs read AND write
                       access there, and switching it off for good means deleting that file too.

This binary serves only when built with the `datafusion-serve` feature, which is what makes the
concrete DataFusion backend nameable:
  cargo run -p vike-datahub --features datafusion-serve";

// The command line, as `clap` reads it. The doc comments on the FIELDS are the `--help` text (that
// is what the derive takes from them), so notes sit in plain comments.
//
// ⚠ **The recording half is parsed in BOTH builds, feature-free, and that is the point.**
// `--record` and its companions are a question about the COMMAND LINE, which every build has;
// whether this binary can ACT on them is a question about the backend, which only a `record` build
// answers — a build that cannot serve still described its own command line correctly, and a build
// that cannot record must describe its own too, then say so at RUN time rather than reject a flag
// it understands.
//
// ⚠ A companion flag without a profile is REFUSED rather than ignored (see [`read_args`]).
#[derive(clap::Parser, Debug)]
#[command(
    about = "The headless data daemon: it serves the hist store over the node protocol and, with a \
             recording profile, ALSO owns the venue subscriptions that fill it. The listen address \
             and the box's store root are settings rows (below). (The COMPUTE verbs are NOT here \
             -- they are served by `vike-backend backtest --addr`, and this daemon refuses them \
             by name.)",
    long_about = None,
    after_help = SETTINGS_HELP,
    disable_help_subcommand = true
)]
pub(super) struct Cli {
    /// Serve (and record into) DIR for THIS run, above the config.store_root row. The way to read
    /// local files: `vike-backend datahub --store DIR` with no node keys authenticates nothing and
    /// binds loopback only.
    #[arg(long, value_name = "DIR")]
    store: Option<PathBuf>,
    /// Record from the `recorder` profile row NAME in the settings store
    /// (<project>/settings/db/vike.db). THE SHIPPED UNIT USES THIS. A name the store does not hold
    /// is REFUSED before the listener binds -- it is never a daemon that comes up healthy and
    /// records nothing. `vike-cli config recorder` lists what the store holds; if it holds none
    /// yet, `vike-cli config bootstrap-recorder <name> --store <root> --venue <v> (--family
    /// <f>|--symbols <a,b,c>)` builds one FROM ARGUMENTS (0086: never from a file) and activates
    /// it. With NO value it records whichever recorder profile is ACTIVE -- the same row `vike-cli`
    /// calls the default, resolved by one shared function so the two sides cannot disagree. If
    /// none is active that is a REFUSAL naming the profiles this store holds, not a silent
    /// serve-only start. RENAMED from `--record-profile`, which STILL WORKS FOR THIS RELEASE and
    /// warns at startup; the NEXT release refuses it and this daemon will not start. Edit the
    /// unit's ExecStart= line now, then systemctl daemon-reload.
    //
    // The OPTIONAL value, and the rule is "a token that is not another flag": a bare
    // `--recorder-profile` means the ACTIVE row (ruling 5), and `--recorder-profile --once` must
    // not record a profile named `--once`. `clap` reads the value as the next token only when it is
    // not a flag, and `--recorder-profile=NAME` states it outright. A profile name that genuinely
    // starts with `-` is therefore unreachable through the space spelling -- an accepted cost.
    #[arg(long = "recorder-profile", value_name = "NAME", num_args = 0..=1)]
    recorder_profile: Option<Option<String>>,
    /// RETIRED (0086): accepted for ONE release and the PATH is IGNORED -- this binary never opens
    /// it. Falls to whichever recorder profile is ACTIVE, the same source a bare
    /// `--recorder-profile` names, behind a startup warning naming the edit to make. A box that
    /// has only ever used `--record` needs a recorder profile ROW before this release reaches it.
    /// Giving both `--record` and `--recorder-profile` is still a refusal; the operator names
    /// which. Omitting both, this daemon serves and records nothing, exactly as it always did.
    #[arg(long, value_name = "PATH")]
    record: Option<String>,
    // The RETIRED spelling of `--recorder-profile`, accepted for one release behind a startup
    // warning (see [`RETIRED_PROFILE_FLAG`]). Hidden: the page names the new spelling only.
    #[arg(long = "record-profile", value_name = "NAME", num_args = 0..=1, hide = true)]
    retired_profile: Option<Option<String>>,
    /// How often to re-resolve the desired symbol set (default 30). Needs a profile.
    #[arg(long = "tick-secs", value_name = "N")]
    tick_secs: Option<u64>,
    /// Warn when a subscribed series has received no rows for N seconds (default 300; 0
    /// disables). Catches a feed that is subscribed and connected but receiving nothing -- which
    /// raises no error anywhere. Needs a recording (--recorder-profile).
    #[arg(long = "silent-secs", value_name = "N")]
    silent_secs: Option<u64>,
    /// Stop with status 3 when a series is silent, so Restart=on-failure can act. OFF by default:
    /// a venue can be legitimately quiet, and a quiet market must not kill a daemon recording
    /// five healthy ones -- and since the merge it would take the data wire down with them.
    /// Alerting (the [alerting] profile table) is the always-on reaction. Needs a recording
    /// (--recorder-profile).
    #[arg(long = "exit-on-silence")]
    exit_on_silence: bool,
    /// Run ONE recording tick and exit -- the dry run. Binds NO listener, so it is safe to run
    /// beside the daemon it is checking. Exits 0 only if EVERY feed ended that tick with at
    /// least one live subscription and no refused subscribe; otherwise status 4, naming each feed
    /// that would record nothing. Stricter than the daemon on purpose: this is a commissioning
    /// check of your own profile, with you watching. Needs a recording (--recorder-profile).
    #[arg(long)]
    once: bool,
}

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
    /// ⚠ It rides the parse rather than being printed inside it, because `read_args` runs before
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
/// from ANY binary, which removes one side of that choice outright — see [`read_args`] for how
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
pub(super) const RETIRED_PROFILE_FLAG: &str = "--record-profile";

/// The flag that names the `recorder` profile row — `--recorder-profile [NAME]`.
///
/// ⚠ **Spelled as a constant because the DEPLOY GATE reads it.**
/// `crates/vike-ops/tests/container_deploy/deploy_layout_gate/unit_fixtures.rs`'s `records` decides whether a shipped unit WRITES
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

/// What the command line asked for: the `--store` it named (if any) and the recording (if a
/// profile was named). Serve-only is `record: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Launch {
    /// The `--store DIR` this command line names, if any: it outranks the `config.store_root` row
    /// for THIS run. Read by a serving build only.
    #[cfg_attr(not(feature = "datafusion-serve"), allow(dead_code, clippy::allow_attributes))]
    pub(super) store: Option<PathBuf>,
    /// The recording asked for, or `None` for a daemon that serves and records nothing.
    pub(super) record: Option<RecordRequest>,
}

/// A usage error found AFTER the parse (a cross-flag rule): `vike-datahub: <message>`, then the
/// usage, exit 2 -- the shape every refusal of the reader has.
fn refusal(program: &Program<'_>, message: &str) -> CliExit {
    let usage = program.command::<Cli>().render_usage();
    CliExit { code: USAGE_EXIT, to_stdout: false, text: format!("{BIN}: {message}\n\n{usage}\n") }
}

/// Read the command line (`argv[0]` already stripped).
///
/// The reader's job is the three things every binary owes a caller -- answer `-h`/`--help`, answer
/// `-V`/`--version`, and REJECT anything else instead of ignoring it (a typo in a systemd
/// `ExecStart=` used to start the server anyway) -- plus the flags that name a recording. A flag
/// that needs an argument and does not get one is an error, never a silently-defaulted knob.
///
/// ⚠ A companion flag without a profile is REFUSED rather than ignored. `--once` alone would
/// otherwise start a server, and `--silent-secs 0` alone would leave an operator believing they had
/// turned a watchdog off on a daemon that was never watching anything.
///
/// # Errors
/// The [`CliExit`] to print and exit with: `-h`/`--help` and `-V`/`--version` (code 0, stdout),
/// every refusal (code 2, stderr).
pub(super) fn read_args(argv: impl IntoIterator<Item = String>) -> Result<Launch, CliExit> {
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let program = Program::new(BIN, &version);
    let cli: Cli = program.try_parse_from(argv)?;
    let refuse = |message: &str| refusal(&program, message);

    // ⚠ A zero tick would spin the resolver flat out against the venue's directory API. Refused
    // before the profile is looked at, as the loop always did.
    if cli.tick_secs == Some(0) {
        return Err(refuse("--tick-secs must be > 0"));
    }
    // Naming more than one profile flag is a refusal, not a precedence question (see
    // [`ProfileSource`]).
    let named =
        [cli.record.is_some(), cli.recorder_profile.is_some(), cli.retired_profile.is_some()];
    if named.into_iter().filter(|given| *given).count() > 1 {
        return Err(refuse(REFUSE_TWO_SOURCES));
    }
    let (profile, retired_flag_warning) =
        match (cli.record, cli.recorder_profile, cli.retired_profile) {
            (Some(path), None, None) => {
                (Some(ProfileSource::ActiveRow), Some(record_warning(&path)))
            }
            (None, Some(name), None) => (Some(source_of(name)), None),
            (None, None, Some(name)) => (Some(source_of(name)), Some(retired_profile_warning())),
            _ => (None, None),
        };
    let store = cli.store;
    let Some(profile) = profile else {
        // The first companion present, in the order the usage lists them.
        let companion = [
            (cli.tick_secs.is_some(), "--tick-secs"),
            (cli.silent_secs.is_some(), "--silent-secs"),
            (cli.exit_on_silence, "--exit-on-silence"),
            (cli.once, "--once"),
        ]
        .into_iter()
        .find_map(|(given, flag)| given.then_some(flag));
        return match companion {
            Some(flag) => Err(refuse(&format!(
                "{flag} configures a recording, so it needs --record PATH or \
                 {RECORDER_PROFILE_FLAG} [NAME]. Without one this daemon only serves the store"
            ))),
            None => Ok(Launch { store, record: None }),
        };
    };
    let silent_secs = cli.silent_secs.unwrap_or(DEFAULT_SILENT_SECS);
    if cli.exit_on_silence && silent_secs == 0 {
        // `--silent-secs 0` disables detection outright, so the exit could never fire -- an
        // operator who wrote both believes something is armed that is not.
        return Err(refuse("--exit-on-silence needs silence detection on (--silent-secs > 0)"));
    }
    Ok(Launch {
        store,
        record: Some(RecordRequest {
            profile,
            tick_secs: cli.tick_secs.unwrap_or(DEFAULT_TICK_SECS),
            silent_secs,
            exit_on_silence: cli.exit_on_silence,
            once: cli.once,
            retired_flag_warning,
        }),
    })
}

/// A profile flag's optional value as a source: a name is a row, no name is the ACTIVE row.
fn source_of(name: Option<String>) -> ProfileSource {
    name.map_or(ProfileSource::ActiveRow, ProfileSource::Row)
}

/// The startup warning `--record PATH` earns: RETIRED (0086), the path is ignored. ⚠ ACCEPTED FOR
/// ONE RELEASE, behind this warning -- see [`ProfileSource`] for why it is not an outright refusal.
fn record_warning(path: &str) -> String {
    format!(
        "--record {path} is RETIRED (decision 0086 — settings live only in the \
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
         `--record {path}`."
    )
}

/// The startup warning the RETIRED `--record-profile` earns: renamed to [`RECORDER_PROFILE_FLAG`].
///
/// ⚠ ACCEPTED FOR ONE RELEASE, LOUDLY -- and the reason is a DEPLOY ORDERING one that a refusal
/// cannot solve. MEASURED 2026-09-22 on the CI box: the live unit spells the retired flag,
/// `/usr/local/sbin/vike-trader-ci-deploy` installs the BINARY and never the unit (it says so
/// itself: "/etc/systemd/system is replaced OUT OF BAND"), and it runs no `daemon-reload`. So a
/// refusal makes BOTH orders fail -- edit the unit first and the CURRENT binary refuses the new
/// spelling at its next restart (`unattended-upgrades` performs one daily); ship first and the new
/// binary refuses the old unit, the health check fails and the deploy auto-rolls-back. Accepting it
/// is therefore the only order-free route, and it does NOT reopen the no-alias rule: the spelling
/// is not an alias to keep, it is a MIGRATION WINDOW with an owner's date on it. The refusal is
/// restored in the release AFTER every unit carries `--recorder-profile`.
fn retired_profile_warning() -> String {
    format!(
        "{RETIRED_PROFILE_FLAG} was RENAMED to `{RECORDER_PROFILE_FLAG}` and is \
         ACCEPTED FOR THIS RELEASE ONLY. Edit this unit's ExecStart= line now: \
         `{RECORDER_PROFILE_FLAG} <name>` names a profile and \
         `{RECORDER_PROFILE_FLAG}` with no value records whichever recorder profile is \
         ACTIVE — then `systemctl daemon-reload`. The next release REFUSES the old \
         spelling and this daemon will not start. The object was already called \
         `recorder` everywhere else (settings/recorder.toml, `vike-cli config \
         recorder`, `vike-cli config mirror --recorder`); this flag was the last holdout"
    )
}

/// Answer `--help`, `--version` and a usage error IDENTICALLY in both builds. A command line that
/// does not parse NEVER starts the daemon: the answer is printed and the process exits (a refusal
/// with status 2). Shared by the two `run`s so the feature can never change what a caller's
/// `--help` does; the returned [`Launch`] carries the recording the caller asked for, if any.
///
/// Help is normal output a user pipes into a pager, so STDOUT and exit 0 -- a non-zero `--help`
/// breaks `set -e`, packaging smoke tests and any wrapper that checks a status. `--version` prints
/// `<name> <version> (<build identity>)`, the shape every `--version` on the box prints (`git
/// version 2.x`) with the COMMIT this binary was built from appended: this server is deployed on
/// the CI box and reached over an SSH tunnel, so "is the running binary the code I pushed?" is asked of
/// it from a terminal (`crates/vike-buildinfo/src/lib.rs` carries the near-miss).
pub(super) fn short_circuit(args: &[String]) -> Launch {
    match read_args(args.iter().cloned()) {
        Ok(launch) => launch,
        Err(exit) => exit.emit(),
    }
}
