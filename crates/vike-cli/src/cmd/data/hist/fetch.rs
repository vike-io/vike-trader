//! `fetch`, `running` and `cancel`: the verbs that reach ONLY a datahub's backfill registry.
use vike_datahub_client::{BackfillDone, DatahubClient};
use vike_model::{parse_date_label, time::epoch_ms_to_utc_date};
use vike_node_proto::auth::{NodeKeys, Scope};

use super::{Args, Window, backfills, fetch_split};
use crate::cmd::data::shared::connect;
use crate::exit::{CliError, CmdResult};

/// Resolve [`Window`]'s two forms to an epoch-ms pair, HERE rather than by forwarding text.
///
/// ⚠ **This is the difference the whole route turns on.** While `fetch` spawned the engine its
/// bounds were forwarded as TEXT and the engine owned the parse. Asking a datahub changes that:
/// `Request::Backfill` carries `start`/`end` as `i64`, so somebody has to parse, and it can only be
/// this side.
///
/// It is NOT a new parser. [`vike_model::parse_date_label`] is the one `universe`
/// ([`membership_window`]), `get` and a remote export already use, which is what keeps `2026-01-01`
/// meaning one thing across those verbs. It takes epoch-ms or a UTC date `YYYY-MM-DD` and REFUSES
/// an hour label — so this verb's help says `YYYY-MM-DD`, and it said `YYYY-MM-DDTHH` until the
/// help was checked against this function (`a_documented_fetch_bound_is_one_the_parser_takes`
/// feeds every documented example through it). ⚠ It is NOT what an ENGINE export's bounds reach;
/// [`membership_window`]'s doc says which parser that is.
///
/// The window this returns is what [`fetch_split::plan`] cuts, so `--days N` is resolved to a
/// window FIRST and then split like any other.
///
/// `--days N` counts back from [`vike_model::now_ms`], the workspace's one sanctioned clock
/// read; `crates/vike-cli/src/lib.rs` already reads it five times, so this adds no
/// `crates/vike-ops/tests/architecture/clock_pin.rs` row.
///
/// # Errors
///
/// An unreadable label, a non-positive `--days`, or an inverted range — the last REFUSED rather
/// than swapped, for [`membership_window`]'s reason: a range whose ends are the wrong way round has
/// two readable meanings and picking one discards half of what the operator typed.
fn fetch_window_ms(window: Option<&Window>) -> Result<(i64, i64), String> {
    let now = vike_model::now_ms();
    match window {
        Some(Window::Days(d)) => {
            let days: i64 =
                d.parse().map_err(|_| format!("--days {d:?} is not a whole number of days"))?;
            if days <= 0 {
                return Err(format!(
                    "--days {days} asks for an empty window; a fetch needs at least one day"
                ));
            }
            Ok((now - days * 86_400_000, now))
        }
        Some(Window::Range { from, to }) => {
            let f = parse_date_label(from).map_err(|e| {
                format!("--from {from:?} is not a timestamp this side can read ({e})")
            })?;
            let t = parse_date_label(to)
                .map_err(|e| format!("--to {to:?} is not a timestamp this side can read ({e})"))?;
            if f >= t {
                return Err(format!(
                    "--from ({}) is not before --to ({}) — an inverted or empty window fetches \
                     nothing, and swapping the ends would discard half of what was typed",
                    epoch_ms_to_utc_date(f),
                    epoch_ms_to_utc_date(t)
                ));
            }
            Ok((f, t))
        }
        // Unreachable by construction: `parse` gives `fetch` a window or refuses the line. Spelled
        // as a refusal rather than an `unwrap` so a future grammar change surfaces here.
        None => Err("fetch needs a window: --days N, or --from/--to".into()),
    }
}

/// Whether `err` — a [`vike_datahub_client::DatahubClient::backfill`] failure's `String` — is that
/// client's OWN client-side refusal because the server never advertised the `backfill` capability
/// AT ALL, as opposed to its SECOND capability check (`backfill_funding` — a server that HAS
/// `backfill` but predates the funding lane) or a server-side `Response::Error` (a funding/spot/
/// unknown-venue refusal from a server that plainly does run `backfill-serve`, since it just
/// answered one of its verbs). D2 (0094 follow-ups): only the FIRST case means the server BINARY
/// lacks the feature, so [`execute_fetch`] appends the `--features backfill-serve` hint only when
/// this returns `true` — it used to append that hint to every failure, which contradicted the
/// funding and server refusals it was pasted onto.
///
/// Matched on the client's own sentence rather than a dedicated error variant (`backfill`'s
/// `Result<_, String>` carries no structure to match on instead). The two capability messages share
/// a prefix and diverge at the very next character after the word `backfill`: this capability's
/// message closes the backtick right there, while the funding capability's continues with
/// `_funding` before its own closing backtick — so a plain substring search on the FIRST message's
/// exact spelling, backtick included, can never match the second.
pub(super) fn is_missing_backfill_feature(err: &str) -> bool {
    err.contains("does not advertise `backfill`")
}

/// `data hist fetch` — ask a datahub to pull a range of history into ITS store.
///
/// # Why this verb has no engine route, when `rm` has both
///
/// `rm` reaches either store because a deletion is meaningful against a local one. A FETCH is not:
/// history is fetched by the backend, once, into the store — *"clients request, never fetch"*, which
/// is [`vike_datahub_client::DatahubClient::backfill`]'s own sentence. The engine route this
/// replaced went straight to a venue's REST from inside `vike-backtest`, which made the compute
/// plane the only crate outside the data plane holding a venue bridge.
///
/// What that buys, measured: the datahub's collector table folds
/// `vike_datahub::backfill::KLINE_SOURCES` — **six venues** against the engine route's one —
/// and it carries the still-forming-candle guard the direct path declares it does not have
/// (`crates/vike-ops/tests/venues/kline_ingest_gate.rs` holds that row).
///
/// ⚠ **What it COSTS, stated because it is a real loss**: `data hist fetch` no longer works with no
/// server. It was a direct venue call and needed nothing; it now needs a reachable datahub — the
/// default `--addr` is `127.0.0.1:7878`. That was ruled deliberately rather than fallen into: a
/// fetch is a data-plane act, and a compute-plane binary reaching a venue directly is the thing
/// being removed.
///
/// [`Scope::Write`] because a backfill WRITES. `crates/vike-datahub-client/src/proto.rs`'s
/// `required_scope` puts it there and argues the boundary: the line that survives scrutiny is
/// bounded-by-an-operator-ceiling versus not, and a backfill spends a venue budget.
///
/// # ⚠ One request, or one per calendar year
///
/// A window LONGER than a year at a venue whose history lane stores whole UTC days under the grid's
/// own keys is sent as one request per calendar year on this one connection, with a line on stderr
/// as each finishes ([`execute_fetch_by_year`]). Every other fetch is the one request it always was,
/// and its output is byte for byte what it always was. [`fetch_split`]'s module doc carries which
/// lanes may be cut, read from each lane's ingest, and why the rest may not: the store dedups by
/// commit key, never by row, so cutting a lane that keys by the request would store the same rows
/// twice.
pub(super) fn execute_fetch(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // Present by construction: `parse` refuses a `fetch` without a spec, and `check_spec` has
    // already held it to three non-empty parts.
    let spec = args.spec.as_deref().unwrap_or_default();
    let mut parts = spec.splitn(3, ':');
    let (venue, symbol, interval) = match (parts.next(), parts.next(), parts.next()) {
        (Some(v), Some(s), Some(i)) => (v, s, i),
        _ => return Err(CliError::usage(format!("'{spec}' is not VENUE:SYMBOL:INTERVAL"))),
    };
    let (start, end) = fetch_window_ms(args.window.as_ref()).map_err(CliError::usage)?;
    let plan = fetch_split::plan(venue, start, end);

    let mut client = connect(&args.addr, keys, Scope::Write)?;
    if plan.is_split() {
        return execute_fetch_by_year(args, &mut client, (venue, symbol, interval), &plan);
    }
    let done = client
        .backfill(venue, symbol, interval, start, end)
        .map_err(|e| CliError::failed(backfill_failure(e, &args.addr)))?;

    // Same stdout/stderr split as the `rm` route above: under `--json` stdout is the document and
    // the human line goes to stderr, so a pipe stays parseable while a person still sees what
    // happened.
    let line = fetched_line((venue, symbol, interval), &done, &args.addr);
    if args.json {
        eprintln!("{line}");
        println!("{}", fetched_json((venue, symbol, interval), &done, &args.addr));
    } else {
        println!("{line}");
    }
    Ok(())
}

/// The sentence a fetch's success ends on — ONE spelling for the one-request route and the per-year
/// one, which appends how many requests it took.
fn fetched_line(
    (venue, symbol, interval): (&str, &str, &str),
    done: &BackfillDone,
    addr: &str,
) -> String {
    let span = match (done.first_ts, done.last_ts) {
        (Some(a), Some(b)) => {
            format!(" spanning {} .. {}", epoch_ms_to_utc_date(a), epoch_ms_to_utc_date(b))
        }
        _ => String::new(),
    };
    format!(
        "fetched {venue}:{symbol}:{interval} -> {} rows written{span} (datahub at {addr})",
        done.rows_written
    )
}

/// The `--json` document a fetch's success emits — the one-request route's whole contract, and the
/// per-year route's BASE, to which it adds one `pieces` array and changes nothing else.
fn fetched_json(
    (venue, symbol, interval): (&str, &str, &str),
    done: &BackfillDone,
    addr: &str,
) -> serde_json::Value {
    serde_json::json!({
        "venue": venue,
        "symbol": symbol,
        "interval": interval,
        "rows_written": done.rows_written,
        "first_ts": done.first_ts,
        "last_ts": done.last_ts,
        "addr": addr,
    })
}

/// A `backfill` call's failure, as the operator reads it.
///
/// ⚠ The `--features backfill-serve` hint is TRUE only for the client's OWN refusal when the server
/// never advertised `backfill` at all ([`vike_datahub_client::DatahubClient::backfill`]'s FIRST
/// capability check). Its SECOND check (`backfill_funding`) and every server-side `Response::Error`
/// — a funding/spot/unknown-venue refusal — mean a `backfill-serve` server DID answer and refused
/// this request for its own reason; appending a "built without the feature" hint to one of those
/// would contradict the sentence right above it.
fn backfill_failure(e: String, addr: &str) -> String {
    if is_missing_backfill_feature(&e) {
        format!(
            "{e}\n  the datahub at {addr} is what fetches now; `vike-cli data hist fetch` no longer \
             reaches a venue itself. A server that does not advertise the verb was built without \
             `--features backfill-serve`."
        )
    } else {
        e
    }
}

/// [`execute_fetch`] for a window [`fetch_split::plan`] cut into per-year pieces: every piece over
/// the ONE connection, a header and one progress line per piece on stderr, and — on success — the
/// same final line and document one request would give, merged over the pieces.
///
/// # What the output is
///
/// * **stderr**, always: the header naming how many requests and why the cut is safe, then one line
///   per finished piece — its window, its rows, how long it took. Progress is stderr because it is
///   not the answer; a pipe reading stdout sees exactly what it saw before.
/// * **stdout**, without `--json`: [`fetched_line`] over the merged result, plus how many requests
///   it took.
/// * **stdout**, with `--json`: [`fetched_json`] over the merged result — every key the
///   one-request document carries, with the same meaning (rows summed, the earliest `first_ts` and
///   the latest `last_ts`) — plus a `pieces` array, one object per request: its own `from`/`to`
///   (inclusive epoch-ms), `rows_written`, `first_ts`, `last_ts` and `elapsed_ms`. A consumer that
///   reads only the one-request keys reads a split run correctly without knowing it was split.
///
/// # A failed piece
///
/// The run STOPS: no later piece is sent. The error names the piece, the pieces before it and the
/// rows they wrote — which stay stored — and how to resume, then the datahub's own text. Under
/// `--json` no document is emitted, the same as every failure on this plane (the module doc's
/// `--json` section); the progress lines of the pieces that finished are already on stderr.
fn execute_fetch_by_year(
    args: &Args,
    client: &mut DatahubClient,
    (venue, symbol, interval): (&str, &str, &str),
    plan: &fetch_split::Plan,
) -> CmdResult<()> {
    let spec = format!("{venue}:{symbol}:{interval}");
    let header = fetch_split::header_line(&spec, venue, plan);
    let outcome = fetch_split::run_pieces(
        plan,
        &header,
        |from, to| {
            client
                .backfill(venue, symbol, interval, from, to)
                .map_err(|e| backfill_failure(e, &args.addr))
        },
        |line| eprintln!("{line}"),
    )
    .map_err(|failed| CliError::failed(failed.message(venue)))?;

    let merged = BackfillDone {
        rows_written: outcome.rows_written,
        first_ts: outcome.first_ts,
        last_ts: outcome.last_ts,
    };
    let line = format!(
        "{}, in {} per-year requests",
        fetched_line((venue, symbol, interval), &merged, &args.addr),
        outcome.pieces.len()
    );
    if args.json {
        eprintln!("{line}");
        let mut doc = fetched_json((venue, symbol, interval), &merged, &args.addr);
        doc["pieces"] = outcome
            .pieces
            .iter()
            .map(|p| {
                serde_json::json!({
                    "from": p.from,
                    "to": p.to,
                    "rows_written": p.done.rows_written,
                    "first_ts": p.done.first_ts,
                    "last_ts": p.done.last_ts,
                    "elapsed_ms": u64::try_from(p.elapsed.as_millis()).unwrap_or(u64::MAX),
                })
            })
            .collect();
        println!("{doc}");
    } else {
        println!("{line}");
    }
    Ok(())
}

// ─── `running` and `cancel`: the door onto fetches a datahub is serving ─────────────────────────

/// `data hist running` — every `fetch` the datahub is running right now.
///
/// [`Scope::Read`]: `Request::ListBackfills` is an Observe-scope verb
/// (`docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`,
/// verdict 2) — it answers from the datahub's registry and changes nothing. A datahub that does not
/// advertise the capability is refused by the client with nothing sent, and that refusal arrives here
/// as a run failure: a box that ANSWERED, for the module doc's exit-ladder reason.
///
/// Under `--json` stdout is the one document and nothing else; the human table is not printed at
/// all, because it carries nothing the document does not.
pub(super) fn execute_running(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let rows = client.list_backfills()?;
    if args.json {
        println!("{}", backfills::running_json(&rows, &args.addr));
    } else {
        for line in backfills::running_lines(&rows, &args.addr) {
            println!("{line}");
        }
    }
    Ok(())
}

/// `data hist cancel SPEC` — stop every running `fetch` on that series at its next chunk boundary.
///
/// [`Scope::Write`]: `Request::CancelBackfill` is Control on a keyed datahub, and a key-less
/// loopback one serves it as it serves `Backfill` (0101's verdict 1). So a box whose CLI holds only
/// the Observe key is refused by the SERVER, by scope, and the refusal names the Control scope.
///
/// # Every answer the datahub serves is `Exit::Ok`
///
/// A cancel that flagged nothing because nothing was running on the series is a SUCCESS: the verb
/// is idempotent and the series is, in fact, not being fetched. So is one whose only match is a
/// one-batch request it cannot stop — the command worked and the output names the request it could
/// not stop and why, which is the answer; a wrapper that wants it stopped regardless has the
/// datahub's restart, which is outside this verb. What is NOT `Ok`: an unreachable datahub
/// (`Exit::Connect`) and a refusal — the capability absent, or the scope — which is a run failure.
///
/// ⚠ It does not WAIT for the flagged requests to stop. Each stops on its own connection's thread
/// at its next boundary, and its OWN client is answered there; `running` is how to watch it go.
pub(super) fn execute_cancel(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // Present by construction: `parse` refuses a `cancel` without a spec, and `check_spec` has
    // already held it to three non-empty parts.
    let spec = args.spec.as_deref().unwrap_or_default();
    let mut parts = spec.splitn(3, ':');
    let (venue, symbol, interval) = match (parts.next(), parts.next(), parts.next()) {
        (Some(v), Some(s), Some(i)) => (v, s, i),
        _ => return Err(CliError::usage(format!("'{spec}' is not VENUE:SYMBOL:INTERVAL"))),
    };
    let mut client = connect(&args.addr, keys, Scope::Write)?;
    let done = client.cancel_backfill(venue, symbol, interval)?;
    if args.json {
        println!("{}", backfills::cancel_json(&done, spec, &args.addr));
    } else {
        for line in backfills::cancel_lines(&done, spec, &args.addr) {
            println!("{line}");
        }
    }
    Ok(())
}

// ─── `import`: a vendor archive, read on the datahub's own box ──────────────────────────────────
