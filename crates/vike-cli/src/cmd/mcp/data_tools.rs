//! The tools that reach the DATA server's history store — `list_series` (a read) and
//! `delete_series` (the one irreversible write on this surface that is not a node command).
//!
//! `delete_series` is an `impl Server` block because its preview-token gate lives on the server
//! (`Server::pending`); `list_series` takes only the address. Split out of `cmd/mcp.rs`
//! (code-layout phase 2, task 9).

use serde_json::{Value, json};
use vike_datahub_client::DatahubClient;

use super::venue_gate::VENUE_CHECK_NONE;
use super::*;
use crate::cmd::verbs;

impl Server {
    /// `delete_series` — DELETE stored history through a datahub, IRREVERSIBLY.
    ///
    /// # ⚠ Why this tool exists at all, and what the guards had to clear
    ///
    /// The design that proposed it opened by EXCLUDING it: a model must not delete data. The owner
    /// reversed that on 2026-09-07 — a user must be able to delete through both the CLI and this
    /// surface — and the exclusion was replaced by GATING rather than by nothing. The argument the
    /// exclusion rested on is what sets the bar: an irreversible delete of market history is a
    /// larger grant than an order, and in one way larger than a credential write — a credential can
    /// be reissued at the venue, and a deleted tape whose venue no longer serves that window cannot
    /// be re-fetched at all.
    ///
    /// So it gets every guard an order gets, plus one an order does not:
    ///
    /// 1. [`WRITE_TOOLS`] membership — the mandatory preview, the `destructiveHint`, the
    ///    `read-only` withholding and the transcript's write classification, all by construction;
    /// 2. the single-use, expiring, INTENT-BOUND `preview_token`;
    /// 3. **`produced_by` is REQUIRED, unconditionally** — see below;
    /// 4. withheld from `read-only` (it is a write) and from `offline` (it opens a socket).
    ///
    /// # ⚠ Guard 3: provenance is UNCONDITIONAL here, unlike on the CLI
    ///
    /// `vike-cli data hist rm` requires `--produced-by` only for a SWEEP, on the argument that deleting
    /// one fully-named series is byte-for-byte what the Data Manager's Delete already does behind a
    /// confirm modal. **That argument does not hold on this surface.** A human naming a series has
    /// SEEN it — the GUI's Delete is reached by clicking a row rendered from the store — while a
    /// model composes the four dimensions from context that may be stale, summarised, or its own
    /// earlier output. A provenance assertion is the one check that fails on a plausible-but-wrong
    /// identity, because a wrongly-named series will not carry the asserted key. It converts the act
    /// from "delete what I named" to "delete what I named, and prove it is what I think it is", and
    /// it costs an agent nothing it should not already have: a model that cannot say which producer
    /// wrote the rows it wants gone does not know enough to delete them.
    ///
    /// There is no override. The CLI's own refusal of a `--force` escape applies here with more
    /// force, not less.
    ///
    /// # The two ways this reaches a server that will not serve it
    ///
    /// ⚠ This heading read "DECLARED GAP: this crate cannot authenticate to a datahub" until #1691,
    /// which closed exactly that: [`Server::datahub_keys`] carries the pair when the box sets it, so
    /// this tool AUTHENTICATES. What remains is not a gap but two legible refusals with different
    /// remedies — this server holding no key (dials unauthenticated, a keyed datahub refuses the
    /// handshake), or the DATAHUB holding none (advertises no delete verb, so
    /// `DatahubClient::delete_series` refuses before a frame is sent). [`DELETE_REMOTE_HINT`] is the
    /// sentence that separates them for whoever hit one.
    pub(super) fn tool_delete_series(&mut self, args: &Value) -> Result<Value, ToolError> {
        use vike_datahub_client::proto::SeriesSelector;

        let intent = delete_intent_from(args).map_err(ToolError::refused)?;
        let selector = SeriesSelector {
            kind: intent.kind.clone(),
            venue: intent.venue.clone(),
            symbol: intent.symbol.clone(),
            group: intent.group.clone(),
            interval: intent.interval.clone(),
        };
        let reason = verbs::reason_from_tool_args(args);
        let token = args.get("preview_token").and_then(Value::as_str);
        let confirmed = args.get("confirm").and_then(Value::as_bool) == Some(true);

        // ⚠ The token is consumed BEFORE the socket is opened, so a stale or mismatched one is
        // refused without a round trip — and, more importantly, without the server ever being asked
        // to plan a deletion nobody may confirm.
        let approved = match token.filter(|_| confirmed) {
            None => None,
            Some(token) => {
                let Some(previewed) = self.pending.take(token) else {
                    return Err(ToolError::refused(format!(
                        "preview_token {token:?} is unknown or already used — a token fires at \
                         most once. Call this tool WITHOUT `confirm` to get a fresh plan, then \
                         confirm with the `preview_token` it returns."
                    )));
                };
                if previewed.issued.elapsed() > PREVIEW_WINDOW {
                    return Err(ToolError::refused(format!(
                        "preview_token {token:?} EXPIRED (previews stay confirmable for {}s). The \
                         store may have changed since it was planned — take a fresh plan.",
                        PREVIEW_WINDOW.as_secs()
                    )));
                }
                if !same_intent(&previewed.intent, &PreviewIntent::Delete(intent.clone())) {
                    return Err(ToolError::refused(
                        "preview_token does not match this deletion — it was issued for a \
                         DIFFERENT selector or a DIFFERENT `produced_by`, and a token is bound to \
                         what it previewed. Plan the deletion you intend, then confirm with THAT \
                         token."
                            .to_string(),
                    ));
                }
                Some(previewed)
            }
        };

        // ⚠ ALWAYS the dry run first, confirmed or not — and its FAILURE does not fail a PREVIEW.
        //
        // That degrade is the same one `Server::node_preview` already makes for an unreachable
        // node, and it is made for the same reason: a preview that could not ask must SAY it could
        // not ask, rather than turning into an error an agent reads as "the tool is broken". A
        // token minted over an unasked store deletes nothing — the confirming call dials again and
        // fails identically — and [`delete_preview`]'s note says so in the same words `preview_of`
        // uses for an unasked node.
        //
        // A CONFIRMING call is the opposite: it must fail, loudly, because it was going to act.
        let asked = self.datahub_for_delete().and_then(|mut client| {
            client
                .delete_series(&selector, Some(&intent.produced_by), true)
                .map_err(|e| ToolError::from(format!("{e}\n{DELETE_REMOTE_HINT}")))
        });

        let Some(_approved) = approved else {
            // A DELETE touches no account, so it stamps no epoch: there is nothing for a
            // changed account set to invalidate about removing a local artifact.
            //
            // ⚠ `None` is that sentence spelled exactly, and it used to be `0` — which claimed the
            // node had ANSWERED zero when nothing had asked it. Inert either way, because the
            // DELETE confirm above never reaches the Node branch's epoch guard, but a sentinel
            // that reads as a measurement is how the guard's own fold started.
            let issued = self.pending.issue(PreviewIntent::Delete(intent.clone()), None);
            let (plan, plan_error) = match asked {
                Ok(planned) => (Some(planned.plan), None),
                Err(e) => (None, Some(e.message)),
            };
            return Ok(delete_preview(
                &intent,
                plan.as_ref(),
                plan_error.as_deref(),
                reason.as_deref(),
                &issued,
            ));
        };
        // Past the gate: the plan pass had to succeed, because the delete pass is about to run
        // against the same server.
        asked?;
        let mut client = self.datahub_for_delete()?;
        let done = client
            .delete_series(&selector, Some(&intent.produced_by), false)
            .map_err(|e| ToolError::from(e.to_string()))?;
        let outcome = done.outcome.unwrap_or_default();
        Ok(json!({
            "will_execute": true,
            "deleted": outcome.deleted.len(),
            "failed": outcome.failed.len(),
            "matched": done.plan.matched(),
            "rows": done.plan.rows(),
            "bytes": done.plan.bytes(),
            "plan": done.plan.lines(),
            "failures": outcome.failed.iter().map(|(id, why)| json!({
                "series": vike_datahub_client::proto::describe_id(id),
                "error": why,
            })).collect::<Vec<_>>(),
            "reason": reason,
            "note": "DELETED — irreversible. A non-empty `failures` list is a PARTIAL run: one \
                     broken series is one skipped series, the rest went, and re-running finishes \
                     the job (the delete is idempotent).",
        }))
    }

    /// The datahub connection `delete_series` needs — AUTHENTICATED when this server holds datahub
    /// keys, and a legible refusal when it does not.
    ///
    /// See [`Server::datahub_keys`] for WHEN `None` happens — a box that set neither datahub key —
    /// and what the unauthenticated fallback then reaches.
    fn datahub_for_delete(&self) -> Result<DatahubClient, ToolError> {
        let addr = &self.datahub_addr;
        match &self.datahub_keys {
            Some(keys) => {
                DatahubClient::connect_authed(addr, keys, vike_node_proto::auth::Scope::Write)
                    .map_err(|e| {
                        ToolError::from(format!("cannot connect to datahub at {addr}: {e}"))
                    })
            }
            None => DatahubClient::connect(addr).map_err(|e| {
                ToolError::from(format!(
                    "cannot connect to datahub at {addr}: {e}\n{DELETE_REMOTE_HINT}"
                ))
            }),
        }
    }
}

/// List every stored series the remote `vike-backend backtest --addr` daemon holds, with its cheap coverage — the
/// absorbed `vike-mcp` `list_series`, gone remote over the datahub `Inventory` metadata verb (the
/// coverage-carrying superset of `ListSeries`, so an agent can pick a `[data]` range, not just a
/// name). Rows are `{kind, venue, symbol, interval, first_ts, last_ts, rows}`.
pub(super) fn tool_list_series(datahub_addr: &str) -> Result<Value, String> {
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let inventory = client.inventory()?;
    let series: Vec<Value> = inventory
        .into_iter()
        .map(|(id, cov)| {
            json!({
                "kind": id.kind, "venue": id.venue, "symbol": id.symbol, "interval": id.interval,
                "first_ts": cov.first_ts, "last_ts": cov.last_ts, "rows": cov.rows,
            })
        })
        .collect();
    Ok(json!({ "series": series }))
}

/// The sentence appended to every `delete_series` connection failure — what an operator can do
/// about it. See [`Server::datahub_keys`].
///
/// ⚠ **It named the wrong remedy between #1688 and #1691.** It read "this server cannot yet
/// authenticate to one" and sent the operator to the box; #1691 made that false, and a model
/// relaying it would have told a human to go SSH somewhere rather than to set the two variables
/// that fix it. The failure it is appended to has two causes with different remedies, so it now
/// separates them: this MCP server holding no key (set them where it launches), or the datahub
/// itself holding none — which advertises no delete verb at all
/// (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`), and no change on this side
/// helps.
const DELETE_REMOTE_HINT: &str = "The delete verb is served ONLY by a datahub that holds node keys. Either set \
     VIKE_DATAHUB_OBSERVE_KEY and VIKE_DATAHUB_CONTROL_KEY where this MCP server launches, so it \
     authenticates, or — if the datahub itself holds none, in which case it advertises no delete \
     verb at all — tell the operator to run it on that box: \
     `vike-cli data hist rm --kind K --venue V --produced-by PREFIX --dry-run` first, then without \
     `--dry-run` and with `--yes`.";

/// Read a `delete_series` call's arguments into a [`DeleteIntent`], refusing every shape the store
/// cannot act on.
///
/// ⚠ **`produced_by` is required HERE**, before anything is dialled and before a token is minted —
/// see [`Server::tool_delete_series`] for why it is unconditional on this surface and conditional on
/// the CLI. What is deliberately NOT checked here is anything needing the store's layout table (an
/// unknown kind, an interval on a kind that does not sub-partition by one): that is
/// `vike_data::store::store_kind`'s to judge, and a roster copied into this crate would be a second list
/// to keep in step — the rule `crate::cmd::data`'s own doc states for `fetch`.
fn delete_intent_from(args: &Value) -> Result<DeleteIntent, String> {
    let required = |name: &str| -> Result<String, String> {
        args.get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("`{name}` is required and must be a non-empty string"))
    };
    let optional = |name: &str| -> Result<Option<String>, String> {
        match args.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim().to_string())),
            // An EMPTY string is refused rather than read as "omitted": an empty `symbol=` is the
            // store's GROUPED-series sentinel, so an empty selector names neither layout. Omitting
            // the field is how a dimension is wildcarded.
            Some(_) => Err(format!(
                "`{name}` must be a non-empty string, or absent to wildcard that dimension"
            )),
        }
    };
    let kind = required("kind")?;
    let venue = required("venue")?;
    let symbol = optional("symbol")?;
    let group = optional("group")?;
    let interval = optional("interval")?;
    let produced_by = args
        .get("produced_by")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(
            "`produced_by` is REQUIRED on this surface, for every call — a sweep and a fully-named \
             series alike. It is the commit-key PREFIX the rows carry, and every key of every \
             matched series must carry it or the whole run is refused. A model composes an \
             identity from context that may be stale; a provenance assertion is the one check that \
             fails on a plausible-but-wrong one. There is no override. Read the prefix off \
             `list_series`, or ask the operator.",
        )?
        .to_string();
    // ⚠ **A producer PATH is refused here, and this surface is the one where it could never have
    // worked.** `--produced-by` has two spellings, and `vike_data::store::store_kind::resolve_produced_by`
    // — which turns a repo-relative producer path INTO its commit-key prefix — used to have exactly
    // one caller in the tree: the ENGINE's local `data rm` arm.
    // `crates/vike-datahub/src/server.rs`'s `delete_series_verb` called nothing of the kind, and
    // this tool is REMOTE-ONLY. So a path sent from here was asserted as a literal prefix, matched
    // no commit key in any series, and came back as "provenance REFUSED" — which reads as a finding
    // about the operator's data rather than as an argument that was never resolved. The schema
    // above promised the resolution unconditionally until this landed; it now says LITERAL, and
    // this is the refusal that makes the boundary visible where it is crossed.
    //
    // ⚠ **The server half landed on 2026-09-11** (`delete_series_verb` now resolves the spelling
    // through the `vike_datahub_client::proto` re-export of that same function), so this refusal is
    // a COMPATIBILITY guard rather than a stand-in — the protocol is capability-negotiated and
    // carries no string for "this server resolves producer paths", so an agent cannot tell a
    // redeployed datahub from an older one. It also stays because the argument for requiring a
    // LITERAL prefix on THIS surface is independent of the server: an agent composes an identity
    // from context that may be stale, and a prefix it read off `list_series` is evidence from the
    // store while a path is a guess about the tree.
    if produced_by.contains('/') {
        return Err(format!(
            "`produced_by` {produced_by:?} looks like a PRODUCER PATH, and this tool deletes \
             through a datahub — one that has not been redeployed since 2026-09-11 resolves none, \
             and this protocol carries no capability string to tell the two apart, so a path would \
             be asserted as a literal prefix, match no commit key, and report the store as \
             foreign. Pass the commit-key PREFIX literally (e.g. `panel_bars:`); `list_series` \
             shows what the rows carry, which is evidence from the store where a path is a guess \
             about a source tree you cannot see."
        ));
    }
    if symbol.is_some() && group.is_some() {
        return Err(
            "`symbol` and `group` are ALTERNATIVES, not a pair: a GROUPED series has an EMPTY \
             symbol and a per-symbol series has no group. Pass one."
                .to_string(),
        );
    }
    if group.is_some() && interval.is_some() {
        return Err(
            "`interval` does not apply to `group`: a grouped series' leaf has no `interval=` \
             segment at all"
                .to_string(),
        );
    }
    for (field, value) in [
        ("kind", Some(&kind)),
        ("venue", Some(&venue)),
        ("symbol", symbol.as_ref()),
        ("group", group.as_ref()),
        ("interval", interval.as_ref()),
    ] {
        let Some(value) = value else { continue };
        if let Some(c) = value.chars().find(|c| "*?[".contains(*c)) {
            return Err(format!(
                "`{field}` value {value:?} contains the glob character {c:?}. Globs are refused: \
                 OMITTING a dimension already wildcards it"
            ));
        }
    }
    Ok(DeleteIntent { kind, venue, symbol, group, interval, produced_by })
}

/// The MANDATORY preview `delete_series` returns for any call that is not a valid confirm.
///
/// # ⚠ What an agent needs that a human does not
///
/// The plan's lines carry the same facts a terminal shows — the store, each matched series with its
/// rows and its commit keys, the totals, the verdict — and that is not enough here. A human reads
/// "2 series, 8,613 rows" and FEELS the size; a model needs `matched`, `rows` and `bytes` as
/// NUMBERS it can compare against what it expected before it decides to confirm. So the totals are
/// typed fields beside the prose, not only inside it.
///
/// The store LEADS, for the reason it leads at a terminal and one more: a human at a shell knows
/// which box they are on, and an agent has no such context at all.
fn delete_preview(
    intent: &DeleteIntent,
    plan: Option<&vike_datahub_client::proto::RemovalPlan>,
    plan_error: Option<&str>,
    reason: Option<&str>,
    preview_token: &str,
) -> Value {
    json!({
        "will_execute": false,
        "tool": "delete_series",
        "selector": {
            "kind": intent.kind,
            "venue": intent.venue,
            "symbol": intent.symbol,
            "group": intent.group,
            "interval": intent.interval,
        },
        "produced_by": intent.produced_by,
        // The TYPED totals, beside the prose and not only inside it — the thing an agent needs that
        // a human does not. `null` when the store could not be asked, never `0`: a fabricated zero
        // is the one answer a model would read as "nothing to delete, proceed".
        "matched": plan.map(|p| p.matched()),
        "rows": plan.map(|p| p.rows()),
        "bytes": plan.map(|p| p.bytes()),
        "plan": plan.map(|p| p.lines()),
        "provenance_satisfied": plan.map(|p| p.verdict().is_ok()),
        "plan_error": plan_error,
        // ⚠ HONESTLY false, always: this tool touches no vike-tradehub node, so there is no node
        // verdict behind it and never will be. The field is carried because it is the one an agent
        // is taught to read as "the verdict that counts", and its ABSENCE on one write tool would
        // be read as a verdict rather than as its absence. What plays that role here is
        // `provenance_satisfied`, which is the STORE's own answer.
        "verified_by_node": false,
        // A store partition is not a mount — see [`VENUE_CHECK_NONE`].
        "venue_check": VENUE_CHECK_NONE,
        "preview_token": preview_token,
        "reason": reason,
        "note": if plan.is_some() {
            "PREVIEW ONLY — nothing was deleted. `plan` is the SERVER's own dry run against the \
             store it has open: which series matched, what each holds, and the commit keys that \
             wrote them. Read `matched`/`rows`/`bytes` and compare them to what you expected BEFORE \
             confirming — a wrong selector usually shows up as a count you did not predict. To \
             execute, call again with BOTH \"confirm\": true AND this exact \"preview_token\". The \
             token fires once, expires after 60s, and is bound to THIS selector AND this \
             `produced_by` — confirming a different deletion with it is refused. Deletion is \
             IRREVERSIBLE, and a window a venue no longer serves cannot be re-fetched."
        } else {
            "PREVIEW ONLY — nothing was deleted. ⚠ THE STORE WAS NOT ASKED (`plan_error` says why), \
             so there is NO plan: `matched`, `rows` and `bytes` are null rather than zero, and you \
             have been shown nothing to confirm. Do not confirm this. A confirming call would fail \
             at the same connection, so nothing can be deleted through it — fix the connection, \
             take a fresh plan, and read it before you decide."
        },
    })
}
