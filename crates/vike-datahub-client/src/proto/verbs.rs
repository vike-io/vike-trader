//! Per-verb tables: which plane serves a verb, which scope may send it, and what it is called.
//!
//! They live in this crate rather than in either server because the data server (`vike-datahub`,
//! layer 65) and the compute server (`vike-backtest`, layer 30) cannot see each other — no edge
//! between them exists or may exist — so a table kept in one would be a second copy in the other,
//! and the first divergence would be a verb BOTH daemons refuse. Every classifier here is an
//! EXHAUSTIVE match with no `_` arm, so a new wire verb fails to COMPILE until somebody says which
//! daemon answers it, which scope may send it and what it is called; WHICH arm was chosen is pinned
//! by this module's tests.

use super::{Request, Scope};

/// Which SERVED SURFACE a verb belongs to — the split ruling 7 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` made, expressed once,
/// BELOW both servers that have to agree about it (the module doc says why here).
///
/// ⚠ The two servers share one [`Request`]/[`Response`](super::Response) SCHEMA on purpose: same
/// `Hello`/`Auth`/`Ping` handshake, same frames, same [`Scope`] rules, same bind posture
/// ([`crate::bind`]). Only which verbs each ANSWERS differs, so one client library, one fixture set
/// and one test harness serve both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    /// The DATA plane — the history/catalog reads, the store writes and the store removal. Served
    /// by `vike-backend datahub`.
    Data,
    /// The COMPUTE plane — the verbs that run an engine (or answer out of the compiled-in strategy
    /// roster). Served by `vike-backend backtest --addr`.
    Compute,
    /// Served by BOTH: the handshake frames and the liveness probe. A daemon that could not answer
    /// `Hello` could not be connected to at all, and `Ping` is how a client learns a socket is
    /// alive before it commits to a verb.
    Shared,
}

impl Plane {
    /// The command that SERVES this plane, for the refusal a client gets when it dialled the other
    /// daemon — a command rather than a port, so an operator can start the missing daemon without
    /// looking anything up.
    pub fn served_by(self) -> &'static str {
        match self {
            Plane::Data => "vike-backend datahub",
            Plane::Compute => "vike-backend backtest --addr",
            // A shared verb is never the subject of a wrong-plane refusal; this keeps the method
            // total.
            Plane::Shared => "either daemon",
        }
    }

    /// The settings key naming the address this plane's daemon is dialled at — the OTHER half of an
    /// actionable refusal: what to start, and where the client is pointing.
    pub fn addr_key(self) -> &'static str {
        match self {
            Plane::Data => "config.datahub_addr",
            Plane::Compute => "config.backtest_addr",
            Plane::Shared => "config.datahub_addr / config.backtest_addr",
        }
    }
}

/// The capability the COMPUTE daemon pushes UNCONDITIONALLY in its `Welcome`
/// (`crates/vike-backtest/src/compute_server/dispatch.rs`'s `served_features` pushes THIS
/// constant), so a client learns BEFORE authenticating which daemon answered.
///
/// ⚠ A constant rather than a literal because something REFUSES on it:
/// `DatahubClient::connect_authed_on` will not sign a key toward a server whose plane
/// [`welcome_plane`] cannot confirm, so a sentinel renamed on one side only would refuse EVERY
/// keyed Studio Run. The VALUE is the frozen token it always was; an older peer compares it
/// literally.
pub const COMPUTE_PLANE_SENTINEL: &str = "backtest";

/// The DATA daemon's unconditional twin of [`COMPUTE_PLANE_SENTINEL`] —
/// `crates/vike-datahub/src/server/features.rs`'s `served_features` pushes this constant. Its
/// presence in a `Welcome` is what [`welcome_plane`] reads as "this socket reaches the store": the
/// plane that carries `Backfill` (a store write spending venue budget) and `DeleteSeries` (the
/// destructive verb) at `VerbScope::Write`.
pub const DATA_PLANE_SENTINEL: &str = "load_bars";

/// The capability the compute daemon pushes exactly when a Studio runner table was MOUNTED — beside
/// `run_sweep` and `run_walkforward` on the same conditional in
/// `crates/vike-backtest/src/compute_server/dispatch.rs`'s `served_features`. A MOUNT fact, so its
/// absence is a different broken route from a wrong daemon: the right daemon, started with nothing
/// to run a Studio slice with.
pub const STUDIO_RUNNER_SENTINEL: &str = "run_slice";

/// **Which plane a server's PRE-AUTH `Welcome` says it serves**, or `None` when the feature list
/// does not name exactly one.
///
/// `Some(Plane::Compute)` needs [`COMPUTE_PLANE_SENTINEL`] AND the absence of
/// [`DATA_PLANE_SENTINEL`]; `Some(Plane::Data)` the mirror image. BOTH present is a pre-split
/// daemon (one process serving both planes, from before ruling 7) and answers `None` deliberately:
/// it serves `Backfill` and `DeleteSeries` too, so a caller that must not open a Write session on
/// the data plane cannot treat it as a compute daemon. NEITHER present is an unknown peer: `None`.
///
/// It reads the `Welcome` because that is the one frame both daemons send before authentication.
/// `Plane::Shared` is never an answer: it classifies VERBS both daemons serve, not a daemon.
pub fn welcome_plane(features: &[String]) -> Option<Plane> {
    let has = |f: &str| features.iter().any(|x| x == f);
    match (has(COMPUTE_PLANE_SENTINEL), has(DATA_PLANE_SENTINEL)) {
        (true, false) => Some(Plane::Compute),
        (false, true) => Some(Plane::Data),
        (true, true) | (false, false) => None,
    }
}

/// Classify one request onto its served surface. **EXHAUSTIVE — no `_` arm, deliberately**, so a
/// new wire verb fails to COMPILE here until somebody says which daemon answers it.
///
/// - **[`Plane::Data`]** — everything that answers out of the STORE or the data daemon's own
///   tables: the `HistStore` reads, the catalog verbs, the [`Request::Backfill`] and
///   [`Request::ImportArchive`] writes and the [`Request::DeleteSeries`] removal.
/// - **[`Plane::Compute`]** — everything that RUNS something: [`Request::RunBacktest`], the Studio
///   [`Request::RunSlice`]/[`Request::RunParamscan`]/[`Request::RunWalkforward`], the
///   profile-shaped [`Request::RunParamscanProfile`]/[`Request::RunWalkforwardProfile`],
///   [`Request::RunStudy`] — and [`Request::ListStrategies`], the row worth arguing: it touches no
///   store and returns `vike_backtest::harness::STRATEGIES`, a compile-time roster, so it sits
///   beside the compute binary's own `--list` flag that reads the same constant.
/// - **[`Plane::Shared`]** — `Hello`, `Auth`, `Ping`.
pub fn plane_of(request: &Request) -> Plane {
    match request {
        Request::Hello { .. } | Request::Auth { .. } | Request::Ping => Plane::Shared,

        Request::RunBacktest(_)
        | Request::RunSlice { .. }
        | Request::RunParamscan { .. }
        | Request::RunWalkforward { .. }
        | Request::RunParamscanProfile { .. }
        | Request::RunWalkforwardProfile { .. }
        | Request::RunStudy(_)
        | Request::ListStrategies
        // ...and the NAMED RUN and its roster verb: unlike their neighbours in SCOPE
        // (`VerbScope::Read`, `docs/decisions/0064-a-named-run-carries-no-source.md`), not in PLANE
        // — they run an engine over the compute daemon's store handle, and enumerate its own
        // roster.
        | Request::NamedStrategies
        | Request::RunNamed(_) => Plane::Compute,

        Request::LoadBars { .. }
        | Request::ScanQuotes { .. }
        | Request::ScanTrades { .. }
        // The SIX tick-level and research reads (`docs/decisions/0084`): `vike_data::HistStore`
        // trait reads answered from the DATA daemon's own store handle.
        | Request::ScanBookUpdates { .. }
        | Request::ScanDepth { .. }
        | Request::ScanCohort { .. }
        | Request::ScanPerpMetrics { .. }
        | Request::ScanEquity { .. }
        | Request::ScanExecFills { .. }
        | Request::PropertiesAsOf { .. }
        | Request::ListSeries
        | Request::Inventory
        | Request::SeriesGaps { .. }
        | Request::SeriesFacts { .. }
        | Request::Coverage
        | Request::Backfill { .. }
        // The DATA daemon's own registry of the `Backfill` requests it is serving.
        | Request::ListBackfills
        | Request::CancelBackfill { .. }
        // Its overlay — mounted lanes, stored credentials, held data — is the DATA daemon's alone.
        | Request::HistoryChannels
        // Writes the DATA daemon's store from the DATA daemon's own imports directory.
        | Request::ImportArchive(_)
        // The DATA daemon's store and collector table, however differently `required_scope` treats
        // it from `Backfill`.
        | Request::SeedSeries { .. }
        // The DATA daemon's provider table and venue egress. It reaches no store, which is
        // `required_scope`'s business: `plane_of` answers WHICH DAEMON — the one that links the
        // bridges.
        | Request::VenueCatalog { .. }
        | Request::DeleteSeries { .. }
        // ⚠ The MARKET-DATA push lane, the highest-consequence row here:
        // `crates/vike-datahub/src/server/connection.rs`'s `handle_connection` asks `plane_of`
        // BEFORE the scope check, so a misclassification would have the DATA daemon refuse its own
        // verbs with a wrong-plane message — everything compiling, nothing working.
        | Request::MdSubscribe { .. }
        | Request::MdUpdate { .. } => Plane::Data,
    }
}

/// The [`Response::Error`](super::Response::Error) text a daemon answers a verb from the OTHER
/// plane with — one spelling, so the two refusals mirror each other and neither drifts into being
/// less useful.
///
/// A wrong-plane dial is a CONFIGURATION mistake, so it names each step of the fix: the verb, the
/// daemon that serves it, and the settings key the client dialled from. It deliberately prints NO
/// address: this side knows only where IT listens, and a tunnelled client's port may differ. It
/// rides an `Error` frame, never a dropped connection — the decode-vs-drop contract.
pub fn wrong_plane_message(verb: &str, serves: Plane, wanted: Plane) -> String {
    format!(
        "{verb} is a {wanted:?}-plane verb and this is the {serves:?} daemon — it is served by \
         `{}`. Point the client at that daemon ({}); this connection stays open and every \
         {serves:?}-plane verb still works on it.",
        wanted.served_by(),
        wanted.addr_key(),
    )
}

/// A request's variant name, for a log line or a refusal — never its PAYLOAD, which on the `Run*`
/// verbs is a client-supplied script and on every verb is remote text. Both daemons log refusals
/// by it; exhaustive for the same reason [`plane_of`] is.
///
/// ⚠ **It returns the WIRE tag, not the Rust identifier**, and the four `sweep` verbs are where the
/// two differ: an operator greps a capture for `RunSweepProfile`, while `RunParamscanProfile`
/// appears on no wire. The tag pins on [`Request::RunParamscan`] and its three siblings are the
/// same decision applied to the bytes; `crates/vike-datahub-client/src/client.rs`'s `resp_kind`
/// follows it for [`Response`](super::Response).
pub fn request_kind(r: &Request) -> &'static str {
    match r {
        Request::Hello { .. } => "Hello",
        Request::Auth { .. } => "Auth",
        Request::Ping => "Ping",
        Request::RunBacktest(_) => "RunBacktest",
        Request::RunSlice { .. } => "RunSlice",
        // ⚠ WIRE tags, not the Rust identifiers — see this function's doc.
        Request::RunParamscan { .. } => "RunSweep",
        Request::RunWalkforward { .. } => "RunWalkforward",
        Request::RunParamscanProfile { .. } => "RunSweepProfile",
        Request::RunWalkforwardProfile { .. } => "RunWalkforwardProfile",
        Request::RunStudy(_) => "RunStudy",
        Request::RunNamed(_) => "RunNamed",
        Request::NamedStrategies => "NamedStrategies",
        Request::LoadBars { .. } => "LoadBars",
        Request::ScanQuotes { .. } => "ScanQuotes",
        Request::ScanTrades { .. } => "ScanTrades",
        Request::ScanBookUpdates { .. } => "ScanBookUpdates",
        Request::ScanDepth { .. } => "ScanDepth",
        Request::ScanCohort { .. } => "ScanCohort",
        Request::ScanPerpMetrics { .. } => "ScanPerpMetrics",
        Request::ScanEquity { .. } => "ScanEquity",
        Request::ScanExecFills { .. } => "ScanExecFills",
        Request::PropertiesAsOf { .. } => "PropertiesAsOf",
        Request::ListSeries => "ListSeries",
        Request::Inventory => "Inventory",
        Request::SeriesGaps { .. } => "SeriesGaps",
        Request::SeriesFacts { .. } => "SeriesFacts",
        Request::ListStrategies => "ListStrategies",
        Request::Coverage => "Coverage",
        Request::Backfill { .. } => "Backfill",
        Request::ListBackfills => "ListBackfills",
        Request::CancelBackfill { .. } => "CancelBackfill",
        Request::HistoryChannels => "HistoryChannels",
        Request::ImportArchive(_) => "ImportArchive",
        Request::SeedSeries { .. } => "SeedSeries",
        Request::VenueCatalog { .. } => "VenueCatalog",
        Request::DeleteSeries { .. } => "DeleteSeries",
        Request::MdSubscribe { .. } => "MdSubscribe",
        Request::MdUpdate { .. } => "MdUpdate",
    }
}

/// The scope a verb requires — the ONE authority for the read/write split on BOTH daemons: the
/// compute daemon enforces the identical rule over the identical [`Scope`] values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbScope {
    /// A pre-auth handshake frame — [`Request::Hello`] / [`Request::Auth`]. Not a verb at all: it
    /// is how a connection GETS a scope, so it cannot require one.
    Handshake,
    /// Servable by a connection authenticated under [`Scope::Read`] or [`Scope::Write`].
    Read,
    /// Servable ONLY by a connection authenticated under [`Scope::Write`].
    Write,
}

/// Classify one request's required scope. **EXHAUSTIVE — no `_` arm, deliberately**: a default of
/// `Read` would leak a write, and a default of `Write` would silently break a read for read-scope
/// clients. Neither is a decision a wildcard should make.
///
/// - **Reads are [`VerbScope::Read`]**: they answer from the store and change nothing.
///   [`Request::ListStrategies`] joins them (a compile-time roster, answered by the compute
///   daemon). `Ping` is `Read`, not Handshake: an unauthenticated liveness probe would be a free
///   oracle for "is this address a vike daemon".
/// - **[`Request::Backfill`] is [`VerbScope::Write`]**: it WRITES the served store and spends
///   venue-API budget from that box's own IP — the "class change" of
///   `docs/decisions/0025-datahub-remote-posture.md`, the reason that record chose authentication.
/// - **[`Request::ImportArchive`] is [`VerbScope::Write`]** too: it writes the store from files on
///   the server's own disk, the client names the window, and a wrong day spends its commit key for
///   good. Its arm carries the argument.
/// - **[`Request::CancelBackfill`] is [`VerbScope::Write`] and [`Request::ListBackfills`] is
///   [`VerbScope::Read`]** —
///   `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`.
///   The cancel writes no row and is still no read: it ends work ANOTHER connection started. The
///   list answers from server state and changes nothing.
/// - **[`Request::HistoryChannels`] is [`VerbScope::Read`]** —
///   `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md`.
/// - **⚠ ALMOST every `Run*` verb is [`VerbScope::Write`], for the SOURCE it can carry rather than
///   the act of running** — the classification a reader most often gets wrong, since these verbs
///   return answers and look like reads. `RunSlice`/`RunSweep`/`RunWalkforward` carry a
///   [`WireSpec`](crate::wire_studio::WireSpec)`::Rhai` source that reaches `StrategySpec::rhai`,
///   and `RunBacktest`/`RunParamscanProfile`/`RunWalkforwardProfile` carry a profile whose
///   `[strategy.params].src` resolves through `vike_backtest::harness::registry`'s `"rhai"` arm to
///   the same compiler: remote code execution BY DESIGN (`vike-cli backtest --script`), whose
///   posture is the write scope. [`Request::RunStudy`] compiles nothing
///   (`vike_studio_core::wire_run`'s `study_run_fn` hardcodes the Rust study tier) and is `Write`
///   for the OTHER reason: it WRITES a run directory on the server's disk
///   (`docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 6).
///
///   ⚠ **And [`Request::RunNamed`] is [`VerbScope::Read`]**, which is 0064's subject. The predicate
///   above names FIELDS: this verb carries [`crate::named_run::NamedParam`] — `i64`/`f64`/`bool`,
///   with no variant a script could occupy — and resolves through a crate that cannot name
///   `vike-script`, so a `src` is UNREAD rather than refused. Its COST bounds (the constants in
///   [`crate::named_run`] plus a server-side run-slot count) are the CONDITION of the
///   classification, not a consequence: 0064's decision 3 found that the denial of service survives
///   removing the compiler. **Moving this line is re-deciding that record, not a
///   reclassification.**
///
///   The cost of the `Write` rows: a `Read` client cannot run a SCRIPT, a SEARCH or a walk-forward,
///   and cannot have a run persisted; it can run one named strategy over one bounded window — the
///   shape `crates/vike-app-core/src/backend/backend_registry.rs`'s `datahub_observe_key` needs, a
///   desktop that must never hold the key which compiles client-supplied Rhai.
pub fn required_scope(request: &Request) -> VerbScope {
    match request {
        Request::Hello { .. } | Request::Auth { .. } => VerbScope::Handshake,

        Request::Ping
        | Request::LoadBars { .. }
        | Request::ScanQuotes { .. }
        | Request::ScanTrades { .. }
        // The SIX tick-level and research reads (`docs/decisions/0084`): `vike_data::HistStore`
        // trait reads, bounded by the range the caller names, writing nothing.
        | Request::ScanBookUpdates { .. }
        | Request::ScanDepth { .. }
        | Request::ScanCohort { .. }
        | Request::ScanPerpMetrics { .. }
        | Request::ScanEquity { .. }
        | Request::ScanExecFills { .. }
        | Request::PropertiesAsOf { .. }
        | Request::ListSeries
        | Request::Inventory
        | Request::SeriesGaps { .. }
        | Request::SeriesFacts { .. }
        // The cross-kind READ behind the Data Manager's "Partial" column: a plain `HistStore` verb.
        | Request::Coverage
        // ⚠ THE MARKET-DATA VERBS ARE READ-SCOPE — a DECISION
        // (`docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md`), not "they look
        // like reads": `Backfill` looks like one too and is Write. The line is BOUNDED BY AN
        // OPERATOR-SET CEILING: a `Backfill`'s venue cost is unbounded and the CLIENT names the
        // range, while a subscription's is bounded by `MD_MAX_KEYS_PER_VENUE` and `MD_LINGER`,
        // which no request moves. Write would force a desktop that only wants a DOM ladder to hold
        // the key that compiles client-supplied Rhai. (On a KEY-LESS server every verb but
        // `DeleteSeries` is served to whoever reaches the loopback socket, per
        // `docs/decisions/0050`.)
        | Request::MdSubscribe { .. }
        | Request::MdUpdate { .. }
        // ⚠⚠ **THE ONE WRITE VERB ON THE READ SIDE, A DECISION AGAINST A STANDING FORWARD RULING.**
        // `docs/decisions/0052`'s reopen list predicted a market-data verb that WRITES would take
        // `Backfill`'s scope; `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` argues
        // the predicate is the CLASS and this verb is not in it — READ IT BEFORE MOVING THIS ARM OR
        // ADDING A FIELD TO THE VARIANT. The short form:
        //   * COST — `SeedSeries` names a series and NOTHING else; the window, bar count, venue
        //     set, interval set, rate and per-process series cap are server constants.
        //   * REACH — the write is ADDITIVE and IDEMPOTENT (commit-key dedup, no existing row is
        //     reachable), CONTAINED (the server's own venue x interval sets), and OPERATOR-ARMED
        //     with an unarmed server still ANSWERING and writing nothing.
        // All four together, or it goes back to Write (0058's decision 3) — the constraint the NEXT
        // write-shaped verb has to argue against. `MdSubscribe` above already spends the same venue
        // budget with NO opt-in at 800 weight/min steady (`crates/vike-datahub/src/md/mod.rs`'s
        // `MD_LINGER`); this lane saturates at 60/min and idles at zero.
        | Request::SeedSeries { .. }
        // ⚠⚠ **THE VENUE CATALOG, READ-SCOPE FOR A DIFFERENT REASON — READ
        // `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` BEFORE
        // MOVING THIS ARM OR ADDING A FIELD TO THE VARIANT.** It does not pass 0058's rule; it is
        // OUTSIDE it:
        //   * NOT A WRITE — nothing enters the served store (no `append_*`, no commit key, no row),
        //     so 0058's rule is INAPPLICABLE rather than satisfied; the list-replace happens only
        //     in the CLIENT's own on-disk cache.
        //   * COST — the request names a VENUE and nothing else, drawn from the gated
        //     `vike_model::VENUES` roster intersected with the server's table: bounded by an
        //     ENUMERABLE SET rather than a rate, which closes the residual 0058 declared for
        //     `SeedSeries`' free symbol.
        //   * CREDENTIALS — the server's table admits only PUBLIC-endpoint providers, so no client
        //     can make this daemon authenticate as its operator (0062 decision 3) — closed by
        //     construction, not by a switch.
        | Request::VenueCatalog { .. }
        | Request::ListStrategies
        // ⚠⚠ **THE NAMED RUN, THE ONLY `Run*` VERB ON THIS SIDE — READ
        // `docs/decisions/0064-a-named-run-carries-no-source.md` BEFORE MOVING THIS ARM OR ADDING
        // A FIELD TO THE VARIANT.** SOURCE: this function's doc (it resolves through
        // `vike_user_strategies::named_run::resolve`, whose crate cannot name `vike-script`). COST,
        // BUILT rather than argued (0064's decision 3): the SINGLE-POINT shape (no grid, no trials,
        // no splits), a window capped by `crate::named_run::NAMED_RUN_MAX_BARS` and REFUSED rather
        // than clamped, a server-owned interval set, the seed/catalog symbol and venue validators,
        // and a process-wide run-slot count that refuses rather than queues. WRITES: nothing —
        // persisting is `RunStudy`, which stays Write (decision 4). These bounds are the CONDITION
        // of this arm; adding a search dimension is 0064's first reopener.
        | Request::RunNamed(_)
        // ...and the roster it enumerates, answerable to the same credential that runs it (0064's
        // decision 7) — otherwise a client names into the dark.
        | Request::NamedStrategies
        // ⚠ THE LIST OF RUNNING BACKFILLS — `docs/decisions/0101`'s verdict 2: it answers from the
        // server's own registry and changes nothing. A read-scope key learns what the operator is
        // fetching and nothing it can act on; a field naming a credential or an account would be
        // 0101's reopener.
        | Request::ListBackfills
        // ⚠⚠ THE HISTORY-CHANNELS READ — READ
        // `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md` BEFORE MOVING THIS
        // ARM OR ADDING A FIELD TO THE VARIANT. Not a write (0058 inapplicable); the server bounds
        // the whole cost (the request names nothing — 0052); it calls no venue (0062 decision 3);
        // and it never USES a credentialed lane's token, only reports its presence as a WORD, so
        // 0097's reopener does not fire. A parameter, a venue call or a value is 0102's reopener.
        | Request::HistoryChannels => VerbScope::Read,

        // The WRITE verb...
        Request::Backfill { .. }
        // ⚠⚠ ...and the verb that STOPS it — READ
        // `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`
        // BEFORE MOVING THIS ARM. It writes no row, but it ends work another connection started
        // (the twin of `record rm`, which 0081 ruled Write), and its whole effect falls on OTHER
        // connections — a read-scope key could stop the operator's backfills while it cannot start
        // one. It DESTROYS nothing, so it takes `Backfill`'s posture, not `DeleteSeries`': served
        // on a key-less LOOPBACK datahub (0101, verdict 1).
        | Request::CancelBackfill { .. }
        // ⚠⚠ ...and the ARCHIVE IMPORT, the second store write — READ
        // `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §2.2 BEFORE MOVING
        // THIS ARM. Write for the reasons the two Read-scope writes above are NOT:
        //   * IT IS A WRITE — quote rows, and bars derived from them, into the SERVED store, which
        //     every reader sees and only `DeleteSeries` removes.
        //   * 0058'S BOUNDED LEG FAILS — the CLIENT names the window (`Backfill`'s reason); a
        //     per-request day cap bounds one request, never the total.
        //   * IT STEERS SERVER-SIDE FILESYSTEM READS, and its failure is IRREVERSIBLE: a mis-scaled
        //     day spends its commit key, and the only remedy is deleting the whole series.
        // It removes nothing, so it takes `Backfill`'s posture: served on a key-less LOOPBACK
        // datahub (`bind_decision` refuses a key-less non-loopback bind), Write-only on a keyed
        // one. A desktop holds a read-scope key, so the CLI is this verb's door.
        | Request::ImportArchive(_)
        // ...and the DESTRUCTIVE one. Write is necessary and NOT sufficient: a KEY-LESS server
        // refuses it outright (see `FEATURE_DELETE_SERIES`); this arm stops a READ-scope connection
        // on a keyed server reaching it.
        | Request::DeleteSeries { .. }
        // ...and the six that compile client-supplied Rhai — see the ⚠ in this function's doc.
        | Request::RunBacktest(_)
        | Request::RunSlice { .. }
        | Request::RunParamscan { .. }
        | Request::RunWalkforward { .. }
        | Request::RunParamscanProfile { .. }
        | Request::RunWalkforwardProfile { .. }
        // A study RUNS code on the backend and WRITES a run directory there;
        // `crates/vike-cli/src/cmd/study.rs`'s connect already negotiates under this scope.
        | Request::RunStudy(_) => VerbScope::Write,
    }
}

/// Whether a connection authenticated under `authed` may send a verb requiring `needed`.
///
/// [`VerbScope::Handshake`] is `false` on purpose: those frames belong to the pre-auth phase, and a
/// SECOND `Hello`/`Auth` on an established connection is a protocol error (a scope that can be
/// re-negotiated after the fact is not a ceiling).
///
/// ⚠ **[`Scope::Account`] admits NOTHING here, deliberately.** It is the TRADEHUB node's scope
/// (`docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` §3c part 2); this
/// server holds no admin key (`node_keys_from_vars` reads two names), so its handshake refuses the
/// scope before any verb. Listing it beside `Write` would grant this service's compute verbs to a
/// capability it cannot authenticate — the enum's own *not a superset*.
pub fn scope_admits(authed: Scope, needed: VerbScope) -> bool {
    match needed {
        VerbScope::Handshake => false,
        VerbScope::Read => matches!(authed, Scope::Read | Scope::Write),
        VerbScope::Write => matches!(authed, Scope::Write),
    }
}

#[path = "verbs_tests.rs"]
#[cfg(test)]
mod verbs_tests;
