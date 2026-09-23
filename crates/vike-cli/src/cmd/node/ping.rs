//! `vike-cli backend ping` — **dial an address and print what the daemon on the other end actually
//! serves.** It computes nothing and places no work: one handshake, one liveness frame, one
//! document.
//!
//! # Why it exists
//!
//! Today an operator cannot ask whether a daemon serves walk-forward, or which store root it
//! opened. The only way to find out is to launch a run and read the refusal — which spends a
//! profile, a round trip and (on the compute plane) a Rhai compile to learn a fact the HANDSHAKE
//! already carried. `Response::Welcome` has advertised the served-verb list since PR-2 and no
//! surface in this tree printed it: `vike_datahub_client::DatahubClient::features` is read only by
//! the client's own per-verb capability checks, each of which turns the answer into a refusal for
//! ONE verb and then throws it away.
//!
//! Two store-root mismatch incidents are on record, and they are the sharper half of the
//! motivation: a daemon that opened a different store than the operator believed answers every
//! query successfully and answers it about the wrong bytes. That question is **not on this wire at
//! all** — see *The store root is a GAP* below, which says exactly what a new verb would need
//! rather than inventing one here.
//!
//! Prior art for the shape: GoCryptoTrader's `btcli getinfo`, `docker info`,
//! `terraform version -json`, `gh api /meta` — a read whose whole product is "what am I talking
//! to".
//!
//! # ⚠ This sub-verb speaks a DIFFERENT PROTOCOL from its four siblings, and that is a wart
//!
//! [`crate::cmd::node`]'s other verbs are about a **`vike-tradehub` node**: `setup` mints that
//! daemon's keys, `connect` attaches this box to one, `status` asks whether the node this box is
//! attached to answers, `disconnect` closes the forward. `ping` asks about an **ADDRESS**, and it
//! is the only verb in the family that does not care which service is behind it: the data server
//! and the compute server share ONE `Request`/`Response` schema, one `Hello`/`Auth`/`Ping`
//! handshake and one [`Scope`] rule — `vike_datahub_client::Plane`'s own doc states that in as many
//! words — so the handshake answers for either without being told which it reached.
//!
//! So **do not read `ping` as `status` with an address, or `status` as `ping` against the
//! configured node.** `status` answers *does the node I am attached to answer, and with which
//! keys*; `ping` answers *what does the daemon at this address serve*. They dial different ports,
//! different protocols and different key pairs (`VIKE_TRADEHUB_*` versus `VIKE_DATAHUB_*`). This
//! item did not fold them and should not: `crate::cmd::datahub`'s module doc argues at length that
//! one verb answering for two services makes both helps false, and the same argument applies to a
//! sub-verb — what makes `ping` admissible here is that it CONFIGURES nothing and WRITES nothing,
//! so it inherits none of the family's per-box rules.
//!
//! # The address ladder is `crate::cmd::backtest::resolve_addr` and nothing else
//!
//! `--addr` → `config.backtest_addr` → `vike_config::DEFAULT_BACKTEST_ADDR`, folded in the ONE
//! function `crate::cmd::strategies` and `crate::cmd::study` already call. That function's own doc
//! refuses a second copy by name: two ladders that disagreed about a blank or padded rung would
//! aim one client at the wrong process, and the three ports in play are one digit apart. A probe is
//! the WORST place to fork it — a verb whose whole job is to say what is at an address must resolve
//! that address the way the verbs it is diagnosing do, or it reports confidently about a socket
//! nobody else dials.
//!
//! The consequence, stated because a default is a claim: a bare `vike-cli backend ping` probes the
//! **compute** daemon. The DATA server is reached by naming it — `--addr`, or the
//! `config.datahub_addr` key this ladder deliberately does not read. That is not a hole in the
//! probe; it is the same rung set every compute dialer in this crate resolves.
//!
//! # ⚠ The store root is a GAP, and no verb was invented for it here
//!
//! **Nothing on this wire answers it.** `vike_datahub_client::Response`'s `Welcome` carries
//! `proto_version`, `features` and (on a keyed server) `nonce` — nothing else — and no
//! `vike_datahub_client::Request` variant asks. On the compute daemon the root is resolved by
//! `crates/vike-backtest/src/binutil.rs`'s `store_root` (`--store` > `$VIKE_HIST_STORE` > the repo
//! checkout) and on the data daemon by `crates/vike-datahub/src/datahub_cli.rs`
//! (`VIKE_DATAHUB_STORE` first); both resolve at a composition root on the daemon's box and neither
//! is ever sent. So this verb reports `store_root` as UNANSWERED rather than guessing — a `null` a
//! consumer might read as "no store" is exactly the confusion `vike_config::CONSUMPTION` exists to
//! prevent, which is why the `--json` document names the question in its `unanswered` list beside
//! the null.
//!
//! What a new wire verb would need, so this is a decision rather than an archaeology exercise:
//!
//! 1. **It may NOT be a field on `Welcome`, and that is the finding.** `Welcome` is PRE-AUTH, and a
//!    store root is a filesystem path on the daemon's box — a directory, often a username,
//!    sometimes a mount point. `vike_datahub_client::required_scope` already puts `Ping` at
//!    `VerbScope::Read` rather than `Handshake` for a strictly weaker disclosure ("a liveness
//!    probe from an unauthenticated peer is a free oracle for *is this address a vike daemon*, and
//!    there is no reason to hand that out"). A path is more than that oracle, so the cheap
//!    `Option` + `skip_serializing_if` shape `Welcome`'s nonce uses is the WRONG shape for this
//!    datum: on a keyed server it would disclose the filesystem layout to any peer that opens a
//!    socket.
//! 2. So: a POST-auth `Request::ServerInfo` → `Response::ServerInfo { store_root, … }` at
//!    `VerbScope::Read`, negotiated through a new `FEATURE_*` capability string and **NOT** by a
//!    `PROTO_VERSION` bump — `vike_datahub_client::FEATURE_STUDY` carries that argument verbatim,
//!    and it is at its strongest here: the version is folded into the signed auth mac
//!    (`vike_node_proto::auth::sign`), so a bump breaks the handshake against every keyed
//!    peer not upgraded in lockstep, and this is an OLD-CLIENT/new-server situation by
//!    construction.
//! 3. `Plane::Shared`, because BOTH daemons open a store. `vike_datahub_client::plane_of` and
//!    `vike_datahub_client::required_scope` are exhaustive matches with no `_` arm, so the new
//!    variant reddens both until classified — and it reddens
//!    `crates/vike-backtest/src/compute_server.rs`'s own exhaustive `handle` match too, which is
//!    why that file is in this wire's diff at all.
//! 4. Both `served_features` push the new string, and each daemon must be handed the root it
//!    actually opened rather than re-resolving one — a second resolution is how the two incidents
//!    above happened in the first place.
//!
//! Until that lands, the probe answers everything the handshake carries and says so about the rest.
//!
//! # ⚠ The plane and studio labels are LITERALS, and nothing holds them equal to the servers'
//!
//! A capability string is a NEGOTIATED TOKEN rather than an identifier — every check in this
//! protocol family is whole-string equality — and `vike_datahub_client::proto` deliberately
//! declares `const`s only for the CAPABILITY strings (`FEATURE_*`), never for the plain verb names
//! the two servers push. Those live as literals in
//! `crates/vike-backtest/src/compute_server.rs`'s `served_features` and
//! `crates/vike-datahub/src/server.rs`'s `served_features`, each of which says in its own doc that
//! the CODE is the authority and that the spellings are frozen (`run_sweep_profile` and `run_sweep`
//! kept their pre-rename names on purpose, and a doc claiming otherwise shipped once already).
//!
//! So [`QUESTIONS`] carries a third copy, and the honest thing is to say what that costs rather
//! than to pretend a test closes it. **It refuses nothing and sends nothing**: a stale literal
//! costs one wrong word in a rendered row, never a wrong refusal and never a wrong frame — and the
//! raw `features` list is printed VERBATIM in both modes right beside the rows, so the daemon's own
//! answer is always available to a reader who distrusts the label. What would close it is a shared
//! `const` per verb name in `proto.rs` with both `served_features` pushing it; that is a
//! three-crate edit against two functions whose docs forbid a second spelling, and it belongs in
//! the same change as the `ServerInfo` verb above, not in a read-only probe.

use vike_datahub_client::DatahubClient;
use vike_node_proto::auth::Scope;

use super::{Args, Ctx};
use crate::exit::{CliError, CmdResult};

/// The capability string the COMPUTE daemon pushes UNCONDITIONALLY —
/// `crates/vike-backtest/src/compute_server.rs`'s `served_features` — so its presence is how a
/// client learns which of the two planes answered. See the module doc's ⚠ for why it is a literal.
const COMPUTE_SENTINEL: &str = "backtest";

/// The DATA daemon's unconditional twin — `crates/vike-datahub/src/server.rs`'s `served_features`.
const DATA_SENTINEL: &str = "load_bars";

/// The family representative for the SIX tick-level and research reads
/// `docs/decisions/0084-only-the-datahub-touches-the-store.md` added — `scan_book_updates`, beside
/// `scan_depth`/`scan_cohort`/`scan_perp_metrics`/`scan_equity`/`scan_exec_fills`.
///
/// ⚠ ONE row rather than six, and the asymmetry with `served_features` (which pushes all six) is
/// deliberate: the wire advertises per VERB so a future server can serve a subset, while this table
/// answers an OPERATOR'S question, and "does this daemon serve the reads my backtest needs" is one
/// question. A server that ever serves a strict subset makes this row a lie and is the event that
/// should split it — `FEATURE_SCAN_BOOK_UPDATES`' own doc argues why six strings exist for that day.
/// These are BUILD facts (plain `HistStore` trait verbs, advertised unconditionally), so the row is
/// the `DATA_SENTINEL` shape rather than the mount-fact `STUDIO_SENTINEL` one.
const SIX_READS_SENTINEL: &str = vike_datahub_client::FEATURE_SCAN_BOOK_UPDATES;

/// The one capability the compute daemon pushes exactly when a Studio runner table was MOUNTED
/// (`run_slice`, beside `run_sweep`/`run_walkforward` on the same conditional). A MOUNT fact, not a
/// build fact: that daemon's `served_features` says so where it pushes them, because the runner
/// comes from `vike-studio-core`, ABOVE `vike-backtest` in the layer graph — so a build that
/// compiled the arm may still have nothing to run.
const STUDIO_SENTINEL: &str = "run_slice";

/// **The questions this probe answers**, in the order it prints them: the operator's words on the
/// left, the negotiated capability string on the right.
///
/// A SLICE rather than a fixed-size array deliberately — a length in a `const` declaration is one
/// more thing to keep in step for a table whose whole purpose is to grow as the wire does.
///
/// The row that earns the table is the first walk-forward one: *does this daemon serve
/// walk-forward* is the question the motivating incident could not ask, and it has TWO answers on
/// this wire — the profile-shaped verb, served on every build, and the Studio DTO verb, served only
/// with a runner table. A single yes/no would be wrong for one of them whichever way it rendered.
const QUESTIONS: &[(&str, &str)] = &[
    ("backtest (profile)", COMPUTE_SENTINEL),
    ("paramscan (profile)", "run_sweep_profile"),
    ("walk-forward (profile)", "run_walkforward_profile"),
    ("search method + multi-rank", vike_datahub_client::FEATURE_SEARCH_METHOD),
    ("studio runners (slice/paramscan/walk-forward)", STUDIO_SENTINEL),
    ("research study", vike_datahub_client::FEATURE_STUDY),
    ("history + catalog reads", DATA_SENTINEL),
    ("tick-level + research reads", SIX_READS_SENTINEL),
    ("coverage", vike_datahub_client::FEATURE_COVERAGE),
    ("backfill on demand", vike_datahub_client::FEATURE_BACKFILL),
    ("delete series", vike_datahub_client::FEATURE_DELETE_SERIES),
    ("market-data push", vike_datahub_client::FEATURE_MARKET_DATA),
];

/// What one probe learned, as DATA — so the rendered report and the `--json` document are two views
/// of ONE answer and cannot come to disagree about a field. The same reason
/// `crate::cmd::strategies`' document renders the values it printed rather than re-deriving them.
#[derive(Debug)]
struct Probe {
    /// The address that was dialled, AFTER the ladder folded. "Which daemon answered" is half the
    /// question the moment more than one exists, and the default rung is invisible in the argv.
    addr: String,
    /// The negotiated protocol version.
    ///
    /// ⚠ This is THIS CLIENT's `vike_datahub_client::PROTO_VERSION`, and reporting it as the
    /// server's is exact rather than approximate: `DatahubClient::connect` compares the two for
    /// STRICT equality and fails the connection — naming both numbers — on any difference, so a
    /// `Probe` exists only where they were equal. The mismatch case never reaches this struct, and
    /// it is already the one failure on this wire that reports both sides by construction.
    proto_version: u32,
    /// The server's advertised capability strings, VERBATIM and in the order it sent them.
    features: Vec<String>,
    /// Whether the server REQUIRES authentication — i.e. it advertises `auth`.
    requires_auth: bool,
    /// The scope this connection actually authenticated under, or `None` against a key-less server
    /// (where no authentication took place). ⚠ NOT the same fact as [`Probe::requires_auth`], and a
    /// report that collapsed them would hide the one configuration an operator most needs to see:
    /// keys held locally against a server that has none.
    authenticated_as: Option<Scope>,
    /// `None` = `Ping` answered; `Some(msg)` = the handshake completed and the liveness frame did
    /// not. Kept as data rather than raised at the socket so the document is still PRINTED — see
    /// [`run_ping`] for why the rung and the document are not alternatives.
    ping_error: Option<String>,
}

impl Probe {
    /// Whether `needle` is in the advertised set. Whole-string equality, which is the ONLY
    /// comparison this protocol family uses — `md_venue=binance` can neither satisfy nor shadow a
    /// named capability, and a `contains` here would break that property.
    fn serves(&self, needle: &str) -> bool {
        self.features.iter().any(|f| f == needle)
    }

    /// Which served surface answered, as a word. `unknown` is a real answer and is printed as one:
    /// a daemon advertising neither sentinel is either older than both spellings or not this
    /// protocol's server at all, and guessing between those is worse than saying so.
    fn plane(&self) -> &'static str {
        match (self.serves(COMPUTE_SENTINEL), self.serves(DATA_SENTINEL)) {
            // ⚠ BOTH is reachable and is not nonsense: the two planes were ONE daemon before ruling
            // 7 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` split
            // them, so a peer predating the split advertises every string. It is named rather than
            // collapsed onto either half, because an operator reading `compute` against such a
            // daemon would conclude the data verbs are elsewhere when they are right there.
            (true, true) => "compute+data (a pre-split daemon)",
            (true, false) => "compute",
            (false, true) => "data",
            (false, false) => "unknown",
        }
    }

    /// Whether a Studio runner table is MOUNTED — see [`STUDIO_SENTINEL`].
    fn studio_mounted(&self) -> bool {
        self.serves(STUDIO_SENTINEL)
    }

    /// The auth posture as ONE sentence, because the two facts are only useful together.
    fn auth_line(&self) -> String {
        match (self.requires_auth, self.authenticated_as) {
            (true, Some(scope)) => format!("REQUIRED — authenticated as {}", scope_word(scope)),
            // Unreachable through `DatahubClient`'s two constructors today (`connect` refuses a
            // keyed server outright, `connect_authed` returns the granted scope or an error), and
            // rendered anyway rather than `unwrap`ped: a probe that panicked on a posture it did
            // not expect would be the worst possible diagnostic for an unexpected posture.
            (true, None) => "REQUIRED — but this connection is NOT authenticated".to_string(),
            (false, Some(scope)) => format!(
                "none advertised — yet this connection authenticated as {} (a server that dropped \
                 its keys mid-life?)",
                scope_word(scope)
            ),
            // ⚠ The line says what a key-less server MEANS rather than only that it is key-less.
            // `docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md` is the posture:
            // every verb but `DeleteSeries` is served to whoever reaches the socket, so the tunnel
            // in front of it is the whole barrier — a fact about the BOX, and the reason this verb
            // prints what it means rather than only the word.
            (false, None) => {
                "none — every verb but DeleteSeries is served to whoever reaches this \
                              socket, so the tunnel in front of it IS the barrier"
                    .to_string()
            }
        }
    }
}

/// The scope as a lowercase word.
///
/// A match rather than `{scope:?}`: the `Debug` rendering of a wire enum is not a promised string,
/// and this one reaches a `--json` document that a script reads.
fn scope_word(scope: Scope) -> &'static str {
    match scope {
        Scope::Read => "observe",
        Scope::Write => "control",
        Scope::Account => "admin",
    }
}

/// The question names this probe could NOT answer from the handshake — see the module doc's GAP
/// section for what each one needs.
///
/// A function rather than a `const` so a future wire verb removes a row by ANSWERING it, which is
/// the only way a consumer's `unanswered` list can be trusted to shrink.
fn unanswered(_probe: &Probe) -> Vec<&'static str> {
    vec!["store_root"]
}

/// The rendered report, as lines. PURE — every cell comes off the [`Probe`], which is what lets the
/// whole rendering be unit-tested with no socket and no daemon.
fn report_lines(probe: &Probe) -> Vec<String> {
    let studio = if probe.studio_mounted() {
        "runner table MOUNTED"
    } else {
        "no runner table — the Studio slice/paramscan/walk-forward verbs are refused here"
    };
    let liveness = match &probe.ping_error {
        None => "Ping answered".to_string(),
        Some(msg) => format!("Ping did NOT answer: {msg}"),
    };
    let mut lines = vec![
        format!("addr:      {}", probe.addr),
        format!(
            "proto:     {}   (negotiated — a completed handshake proves both sides agree)",
            probe.proto_version
        ),
        // ⚠ `plane:` and not `serves:` — the SECTION header further down is `serves:`, and two
        // labels one word apart in one document is how a reader comes to answer the wrong question
        // from the right output. The value is the bare token the `--json` field carries, so the two
        // renderings say the same word.
        format!("plane:     {}", probe.plane()),
        format!("auth:      {}", probe.auth_line()),
        format!("liveness:  {liveness}"),
        format!("studio:    {studio}"),
    ];

    let venues = vike_datahub_client::advertised_md_venues(&probe.features);
    if !venues.is_empty() {
        lines.push(format!("md venues: {}", venues.join(", ")));
    }

    lines.push(String::new());
    lines.push("serves:".to_string());
    for &(question, capability) in QUESTIONS {
        let answer = if probe.serves(capability) { "yes" } else { "no" };
        lines.push(format!("  {question:<46} {answer:<4}  ({capability})"));
    }

    lines.push(String::new());
    // ⚠ VERBATIM, and this is the load-bearing line rather than a dump: every row above is this
    // client's READING of these strings, and the module doc admits nothing holds those readings
    // equal to the servers' spellings. Printing the daemon's own answer beside them is what lets a
    // reader who distrusts a label check it without reaching for a second tool.
    lines.push(format!("features:  {}", probe.features.join(", ")));

    lines.push(String::new());
    for question in unanswered(probe) {
        lines.push(format!(
            "unanswered: {question} — no verb on this wire answers it. `vike-cli backend ping \
             --help` says what one would need; nothing is guessed here."
        ));
    }
    lines
}

/// The `--json` document. Same [`Probe`], same facts, machine-shaped.
///
/// ⚠ `store_root` is PRESENT and `null`, and `unanswered` names it. A key that were simply ABSENT
/// would let a consumer's lookup fall through to a default, and a `null` alone reads as "this
/// daemon has no store root" — which is false. Naming the question is what distinguishes *not
/// answered* from *answered with nothing*, the distinction this workspace re-learns most often.
fn probe_json(probe: &Probe) -> String {
    // Hoisted rather than inlined into the macro: `json!` munches an object's values as token
    // trees up to the next top-level comma, so a `match` in a value position is legible to the
    // macro and illegible to the next reader. One binding each is cheaper than both.
    let ping = probe.ping_error.as_deref().unwrap_or("ok");
    let venues = vike_datahub_client::advertised_md_venues(&probe.features);
    let doc = serde_json::json!({
        "addr": probe.addr,
        "proto_version": probe.proto_version,
        "features": probe.features,
        "plane": probe.plane(),
        "studio_runners": probe.studio_mounted(),
        "auth_required": probe.requires_auth,
        "authenticated_as": probe.authenticated_as.map(scope_word),
        "ping": ping,
        "md_venues": venues,
        "store_root": serde_json::Value::Null,
        "unanswered": unanswered(probe),
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

/// Dial, handshake, ping, print. The ONLY I/O in this module.
///
/// # The rungs, and why there are three of them
///
/// * **A socket that never answered is `crate::exit::Exit::Connect`** — the rung a wrapper backs
///   off and retries on, classified at the socket where the address is still in scope so the
///   message can name what was unreachable. That is `crate::cmd::strategies`' spelling and this
///   verb matches it deliberately: a probe and the verbs it diagnoses must agree about what
///   "unreachable" means, or a script branching on the probe's answer branches wrong.
/// * **A KEYED server this box holds no keys for is `crate::exit::Exit::Failed`**, and this is the
///   classification worth arguing. `DatahubClient::connect` completes the handshake, sees `auth`
///   advertised, and only then refuses — with `io::ErrorKind::PermissionDenied`. So **the daemon
///   answered**, and re-running unchanged cannot succeed until a key exists. Both halves rule out
///   rung 3, whose whole promise is "the same invocation may well work later". It is the rule
///   `crate::cmd::node::connect`'s `verify` applies one protocol over: a reachable box that refused
///   is a configuration fact, not a transport failure.
/// * **A completed handshake whose `Ping` did not come back prints the document AND exits on
///   `crate::exit::Exit::Connect`.** Those are not alternatives: the document is this verb's
///   product — the argument `crate::exit::Exit::Breach`'s doc makes for the gate verb, applied here
///   — so it goes to stdout in full, and the rung goes beside it on stderr.
pub(super) fn run_ping(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let addr = crate::cmd::backtest::resolve_addr(args.addr.as_deref(), ctx.backtest_addr);

    let mut client = match ctx.datahub_keys {
        // ⚠ `Scope::Read`, the weakest ceiling that can send anything at all. This verb's
        // heaviest frame is `Request::Ping`, which `vike_datahub_client::required_scope` puts at
        // `VerbScope::Read` — deliberately, rather than at `Handshake`, so an unauthenticated
        // peer gets no free "is this a vike daemon" oracle. Negotiating `Control` to read a
        // handshake would hand this process the authority to compile Rhai on the far side for
        // nothing at all.
        Some(keys) => DatahubClient::connect_authed(&addr, keys, Scope::Read),
        None => DatahubClient::connect(&addr),
    }
    .map_err(|e| classify_dial_failure(&addr, &e))?;

    // The handshake is over, so the server's own answers are already captured. They are read BEFORE
    // the ping, so a broken liveness frame cannot cost the document.
    let mut probe = Probe {
        addr: addr.clone(),
        proto_version: vike_datahub_client::PROTO_VERSION,
        features: client.features().to_vec(),
        requires_auth: client.requires_auth(),
        authenticated_as: client.authenticated_scope(),
        ping_error: None,
    };
    probe.ping_error = client.ping().err().map(|e| e.to_string());

    if args.json {
        println!("{}", probe_json(&probe));
    } else {
        for line in report_lines(&probe) {
            println!("{line}");
        }
    }

    if let Some(msg) = &probe.ping_error {
        return Err(CliError::connect(format!(
            "{addr} completed the handshake and then did not answer a Ping: {msg}. The document \
             above is what the handshake carried; treat everything else as unknown."
        )));
    }
    Ok(())
}

/// Turn a dial failure into a rung — see [`run_ping`]'s rung section for the whole argument.
///
/// A free function so the classification is unit-testable without a socket: the property worth
/// holding is that a REFUSED-BY-POLICY answer and an UNREACHABLE socket never share a rung, and
/// asserting that has to be possible without a keyed daemon to hand.
fn classify_dial_failure(addr: &str, e: &std::io::Error) -> CliError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return CliError::failed(format!(
            "{addr} ANSWERED and requires authentication, and this box resolved no vike-datahub \
             node keys: {e}\n`vike-cli datahub setup` mints the pair on the SERVER's box; \
             VIKE_DATAHUB_OBSERVE_KEY / VIKE_DATAHUB_CONTROL_KEY in the store `vike-cli secrets \
             path` prints is where this box reads them.\n(⚠ A local firewall rule can produce this \
             same error kind with no daemon there at all — check the port is open before rotating \
             a key.)"
        ));
    }
    CliError::connect(format!(
        "cannot reach a vike-datahub-protocol daemon at {addr}: {e} (the compute plane is served \
         by `vike-backend backtest --addr`, the data plane by `vike-backend datahub`)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(features: &[&str]) -> Probe {
        Probe {
            addr: "127.0.0.1:7880".to_string(),
            proto_version: vike_datahub_client::PROTO_VERSION,
            features: features.iter().map(|s| (*s).to_string()).collect(),
            requires_auth: features.contains(&vike_datahub_client::FEATURE_AUTH),
            authenticated_as: None,
            ping_error: None,
        }
    }

    fn json_of(probe: &Probe) -> serde_json::Value {
        serde_json::from_str(&probe_json(probe)).expect("the document is valid JSON")
    }

    /// The two sentinels classify the two planes, a pre-split daemon is NAMED rather than
    /// collapsed, and a daemon advertising neither is `unknown` rather than guessed at.
    #[test]
    fn the_plane_is_read_off_each_daemons_unconditional_capability() {
        assert_eq!(probe(&["backtest", "list_strategies"]).plane(), "compute");
        assert_eq!(probe(&["load_bars", "inventory"]).plane(), "data");
        assert_eq!(probe(&["backtest", "load_bars"]).plane(), "compute+data (a pre-split daemon)");
        assert_eq!(probe(&[]).plane(), "unknown");
        assert_eq!(probe(&["something_else"]).plane(), "unknown");
    }

    /// The Studio answer is the MOUNT sentinel and nothing else — a compute daemon with no runner
    /// table must read "not mounted" even though it serves every profile-shaped verb.
    #[test]
    fn the_studio_table_is_mounted_only_when_its_own_capability_is_advertised() {
        assert!(!probe(&["backtest", "run_walkforward_profile"]).studio_mounted());
        assert!(probe(&["backtest", "run_slice", "run_sweep", "run_walkforward"]).studio_mounted());
    }

    /// ⚠ Capability matching is WHOLE-STRING, and this is the assertion that keeps it so: a
    /// `contains`-shaped check would let `run_slice_v2` satisfy `run_slice`, and the per-venue
    /// `md_venue=` entries exist only because every check in this protocol family is equality.
    #[test]
    fn a_capability_is_matched_whole_and_never_as_a_substring() {
        assert!(!probe(&["run_slice_v2"]).studio_mounted());
        assert!(!probe(&["md_venue=backtest"]).serves(COMPUTE_SENTINEL));
        assert!(probe(&["run_slice"]).studio_mounted());
    }

    /// **`requires_auth` and `authenticated_as` are different facts**, and the report may not
    /// collapse them: keys held locally against a key-less server is the configuration an operator
    /// most needs to SEE, because every verb is being served to whoever reaches the socket.
    #[test]
    fn the_auth_line_separates_what_the_server_demands_from_what_this_connection_got() {
        let keyless = probe(&["backtest"]);
        assert!(keyless.auth_line().contains("none"), "{}", keyless.auth_line());
        assert!(
            keyless.auth_line().contains("DeleteSeries"),
            "a key-less posture must say what it MEANS: {}",
            keyless.auth_line()
        );

        let mut keyed = probe(&["backtest", vike_datahub_client::FEATURE_AUTH]);
        keyed.authenticated_as = Some(Scope::Read);
        assert!(keyed.auth_line().contains("REQUIRED"), "{}", keyed.auth_line());
        assert!(keyed.auth_line().contains("observe"), "{}", keyed.auth_line());

        // …and the pair that should not happen is RENDERED rather than panicked on.
        let mut odd = probe(&["backtest"]);
        odd.authenticated_as = Some(Scope::Write);
        assert!(odd.auth_line().contains("control"), "{}", odd.auth_line());
    }

    /// Every question renders a row, in the table's own order, and each row names the capability it
    /// was read from — so a reader can check a label against the verbatim `features` line below it.
    #[test]
    fn every_question_renders_a_row_naming_the_capability_it_read() {
        let text = report_lines(&probe(&["backtest", "run_slice"])).join("\n");
        for &(question, capability) in QUESTIONS {
            assert!(text.contains(question), "the report omits {question}:\n{text}");
            assert!(text.contains(capability), "the report omits {capability}:\n{text}");
        }
        assert!(text.contains("features:"), "the verbatim list is the check on every row: {text}");
    }

    /// The UNANSWERED question is named in both renderings, and the JSON carries `store_root` as an
    /// explicit `null` BESIDE the name — see [`probe_json`] for why an absent key and a bare null
    /// are both wrong.
    #[test]
    fn the_store_root_is_reported_as_unanswered_and_never_guessed() {
        let p = probe(&["backtest"]);
        let text = report_lines(&p).join("\n");
        assert!(text.contains("unanswered: store_root"), "{text}");

        let doc = json_of(&p);
        assert_eq!(doc["store_root"], serde_json::Value::Null);
        assert_eq!(doc["unanswered"], serde_json::json!(["store_root"]));
    }

    /// The document carries the address it dialled and the negotiated version — "which daemon
    /// answered" is half the answer once more than one exists, and the ladder's default rung is
    /// invisible in the argv.
    #[test]
    fn the_json_document_names_the_address_the_version_and_the_plane() {
        let mut p = probe(&["backtest", "run_slice", vike_datahub_client::FEATURE_AUTH]);
        p.authenticated_as = Some(Scope::Write);
        let doc = json_of(&p);
        assert_eq!(doc["addr"], serde_json::json!("127.0.0.1:7880"));
        assert_eq!(doc["proto_version"], serde_json::json!(vike_datahub_client::PROTO_VERSION));
        assert_eq!(doc["plane"], serde_json::json!("compute"));
        assert_eq!(doc["studio_runners"], serde_json::json!(true));
        assert_eq!(doc["auth_required"], serde_json::json!(true));
        assert_eq!(doc["authenticated_as"], serde_json::json!("control"));
        assert_eq!(doc["ping"], serde_json::json!("ok"));
        assert_eq!(doc["features"].as_array().map(Vec::len), Some(3));
    }

    /// A key-less server's document says `null` for the scope rather than inventing one, and a
    /// failed Ping travels as its MESSAGE rather than as a boolean.
    #[test]
    fn a_keyless_server_and_a_failed_ping_both_travel_honestly() {
        let mut p = probe(&["load_bars"]);
        p.ping_error = Some("timed out".to_string());
        let doc = json_of(&p);
        assert_eq!(doc["authenticated_as"], serde_json::Value::Null);
        assert_eq!(doc["auth_required"], serde_json::json!(false));
        assert_eq!(doc["ping"], serde_json::json!("timed out"));
        assert!(report_lines(&p).iter().any(|l| l.contains("Ping did NOT answer")), "{p:?}");
    }

    /// The per-venue market-data entries are DECODED through the protocol's own helper rather than
    /// by a second prefix parser here, and they are omitted entirely when none was advertised — an
    /// empty row would read as "this daemon serves market data for nothing".
    #[test]
    fn advertised_market_data_venues_are_decoded_by_the_protocols_own_helper() {
        let p = probe(&[
            "load_bars",
            vike_datahub_client::FEATURE_MARKET_DATA,
            "md_venue=binance",
            "md_venue=bybit",
        ]);
        let text = report_lines(&p).join("\n");
        assert!(text.contains("md venues:") && text.contains("binance"), "{text}");
        assert_eq!(json_of(&p)["md_venues"], serde_json::json!(["binance", "bybit"]));

        let none = probe(&["load_bars"]);
        assert!(!report_lines(&none).iter().any(|l| l.contains("md venues")), "{none:?}");
        assert_eq!(json_of(&none)["md_venues"], serde_json::json!([]));
    }

    /// **A REFUSED-BY-POLICY answer and an UNREACHABLE socket never share a rung**, which is the
    /// one property a wrapper branches on: rung 3 promises "the same invocation may well work
    /// later" and a keyed daemon this box has no key for will not.
    #[test]
    fn a_keyed_refusal_and_an_unreachable_socket_land_on_different_rungs() {
        use crate::exit::Exit;
        use std::io::{Error, ErrorKind};

        let keyed = Error::new(ErrorKind::PermissionDenied, "keyed");
        let refused = classify_dial_failure("1.2.3.4:9", &keyed);
        assert_eq!(refused.exit, Exit::Failed);
        let msg = refused.msg;
        assert!(msg.contains("ANSWERED"), "{msg}");
        assert!(msg.contains("vike-cli datahub setup"), "it names the minter: {msg}");
        assert!(msg.contains("firewall"), "the residual is declared where it bites: {msg}");

        let dead = Error::new(ErrorKind::ConnectionRefused, "nothing there");
        let unreachable = classify_dial_failure("1.2.3.4:9", &dead);
        assert_eq!(unreachable.exit, Exit::Connect);
        let msg = unreachable.msg;
        assert!(msg.contains("1.2.3.4:9"), "{msg}");
        assert!(msg.contains("vike-backend"), "it names what to start: {msg}");
    }
}
