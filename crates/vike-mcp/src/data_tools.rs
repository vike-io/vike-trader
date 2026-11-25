//! The tools that reach the DATA server's history store — `list_series` (a read) and
//! `delete_series` (the one irreversible write on this surface that is not a node command).
//!
//! `delete_series` is an `impl Server` block because its preview-token gate lives on the server
//! (`Server::pending`); `list_series` takes only the address. Split out of `src/lib.rs`
//! (code-layout phase 2, task 9).

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use vike_data::store::removal::{
    RemovalPlan, SeriesSelector, account_data_refusal, produced_by_refusal,
};
use vike_data::{SeriesCoverage, SeriesId};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::client::data_ops::DeleteRefusal;
use vike_model::StorePlane;

use super::tool_args::{DeleteSeriesArgs, WriteGate, parse_args};
use super::venue_gate::VENUE_CHECK_NONE;
use super::*;

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
    /// `vike-cli data history rm` requires `--produced-by` only for a SWEEP, on the argument that deleting
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
    /// which closed exactly that: [`Server::datahub_keys`] carries this box's device key when it holds
    /// one, so this tool AUTHENTICATES. What remains is not a gap but two legible refusals with different
    /// remedies — this server holding no key (dials unauthenticated, a keyed datahub refuses the
    /// handshake), or the DATAHUB holding none (advertises no delete verb, so
    /// `DatahubClient::delete_series` refuses before a frame is sent). [`DeleteRefusal`] is the
    /// sentence that separates them for whoever hit one, the one `vike-cli data history rm --addr` shows.
    pub(super) fn tool_delete_series(&mut self, args: &Value) -> Result<Value, ToolError> {
        let (intent, selector) = delete_intent_from(args).map_err(ToolError::refused)?;
        let gate = WriteGate::of(args);
        let reason = gate.reason();

        // ⚠ The token is consumed BEFORE the socket is opened, so a stale or mismatched one is
        // refused without a round trip — and, more importantly, without the server ever being asked
        // to plan a deletion nobody may confirm.
        let approved = match gate.confirmed_token() {
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
                .map_err(|e| ToolError::from(delete_failure(&e, false)))
        });

        let Some(_approved) = approved else {
            // A DELETE touches no account, so it stamps no epoch: there is nothing for a
            // changed account set to invalidate about removing a local artifact.
            //
            // ⚠ `None` is that sentence spelled exactly, and it used to be `0` — which claimed the
            // node had ANSWERED zero when nothing had asked it. Inert either way, because the
            // DELETE confirm above never reaches the Node branch's epoch guard, but a sentinel
            // that reads as a measurement is how the guard's own fold started.
            let (plan, plan_error) = match asked {
                Ok(planned) => (Some(planned.plan), None),
                Err(e) => (None, Some(e.message)),
            };
            // A plan whose provenance the store REFUSED has nothing to confirm: no token, so no second call can
            // delete through it (the CLI refuses before its confirm too). A plan the store could not answer keeps
            // its token: the confirm fails at the same connection, and the note says not to.
            let refused = plan.as_ref().and_then(RemovalPlan::refusal);
            let issued = refused
                .is_none()
                .then(|| self.pending.issue(PreviewIntent::Delete(intent.clone()), None));
            return Ok(delete_preview(
                &intent,
                plan.as_ref(),
                plan_error.as_deref(),
                reason.as_deref(),
                issued.as_deref(),
                refused.as_ref(),
            ));
        };
        // Past the gate: the plan pass had to succeed, because the delete pass is about to run
        // against the same server, and its provenance is asked again: the store may have moved since
        // the token was issued.
        if let Some(refusal) = asked?.plan.refusal() {
            return Err(ToolError::refused(format!(
                "{} The preview_token is spent and nothing was deleted: the store changed since the plan. \
                 Change `produced_by` or the selector, and plan again.",
                refusal.rendered()
            )));
        }
        let mut client = self.datahub_for_delete()?;
        // ⚠ A delete sent and never answered may have removed rows (decision 0303's OUTCOME UNKNOWN), decided on
        // the failure's type in `DeleteRefusal::of`, the words `vike-cli data history rm --addr` shows too.
        let done = client
            .delete_series(&selector, Some(&intent.produced_by), false)
            .map_err(|e| ToolError::from(delete_failure(&e, true)))?;
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
                "series": vike_data::store::removal::describe_id(id),
                "error": why,
            })).collect::<Vec<_>>(),
            "reason": reason,
            "note": "DELETED — irreversible. A non-empty `failures` list is a PARTIAL run: one \
                     broken series is one skipped series, the rest went, and re-running finishes \
                     the job (the delete is idempotent).",
        }))
    }

    /// The datahub connection `delete_series` needs — AUTHENTICATED when this box holds a device
    /// key, and a legible refusal when it does not.
    ///
    /// See [`Server::datahub_keys`] for WHEN `None` happens — a box with no device key — and what
    /// the unauthenticated fallback then reaches.
    fn datahub_for_delete(&self) -> Result<DatahubClient, ToolError> {
        let addr = &self.datahub_addr;
        let scope = vike_hub_protocol::auth::Scope::Write;
        DatahubClient::connect_with_keys(addr, self.datahub_keys.as_ref(), scope).map_err(|e| {
            let failure = format!("cannot connect to datahub at {addr}: {e}");
            ToolError::from(DeleteRefusal::shown(&e, failure, false))
        })
    }
}

/// What a failed `delete_series` exchange says: [`DeleteRefusal::shown`], the words `vike-cli data history rm
/// --addr` shows. `deleting` is the confirming pass, the one whose lost answer may have removed rows.
fn delete_failure(error: &vike_hub_protocol::call::HubCallError, deleting: bool) -> String {
    DeleteRefusal::shown(error, error.to_string(), deleting)
}

/// The most rows [`tool_list_series`] answers. Claude Code saves a text result over 50,000 characters
/// to a file instead of showing it, and a rendered row is about 190 characters, about 270 with a
/// Polymarket token id for a symbol, so this stays inline in the worst case. `tools_spec`'s
/// description states the number and `list_series_is_bounded_and_says_so` holds the two equal.
pub(super) const LIST_SERIES_LIMIT: usize = 150;

/// List the stored series the remote datahub holds, with its cheap coverage — the
/// absorbed `vike-mcp` `list_series`, gone remote over the datahub `Inventory` metadata verb (the
/// coverage-carrying superset of `ListSeries`, so an agent can pick a `[data]` range, not just a
/// name), answered by [`series_answer`]. Dialled the way `vike-cli data history ls` dials: under the
/// box's keys when it holds them.
pub(super) fn tool_list_series(
    datahub_addr: &str,
    keys: Option<&vike_hub_protocol::auth::HubKeys>,
    reply_wait: Duration,
) -> Result<Value, String> {
    let scope = vike_hub_protocol::auth::Scope::Read;
    let mut client = DatahubClient::connect_with_keys(datahub_addr, keys, scope)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    client.set_reply_timeout(Some(reply_wait));
    Ok(series_answer(client.inventory()?))
}

/// The answer for an inventory: each MARKET series `{kind, venue, symbol, group, interval, first_ts, last_ts,
/// rows}` (`group` set and `symbol` empty for a grouped one, `SeriesId`'s two layouts), cut by
/// [`bounded_series`].
///
/// ⚠ **An ACCOUNT series is withheld and COUNTED, as `vike-cli data history ls` withholds it**
/// (`crates/vike-cli/src/cmd/data/hist/list.rs`'s `execute_list`): the one classification both read is the model's
/// `StoreKindId::plane`. It is your own activity (fills, orders, funding payments, equity), not market data, and a
/// silent filter would under-report the store, so `withheld_account_series` says how many and of which kinds.
pub(super) fn series_answer(inventory: Vec<(SeriesId, SeriesCoverage)>) -> Value {
    let (account, market): (Vec<_>, Vec<_>) =
        inventory.into_iter().partition(|(id, _)| id.kind.plane() == StorePlane::Account);
    let series: Vec<Value> = market
        .into_iter()
        .map(|(id, cov)| {
            json!({
                "kind": id.kind, "venue": id.venue, "symbol": id.symbol, "group": id.group,
                "interval": id.interval, "first_ts": cov.first_timestamp,
                "last_ts": cov.last_timestamp, "rows": cov.rows,
            })
        })
        .collect();
    let withheld_account_series = (!account.is_empty()).then(|| {
        let mut kinds: Vec<&str> = account.iter().map(|(id, _)| id.kind.word()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        json!({
            "count": account.len(),
            "kinds": kinds,
            "note": "account data (your own fills, orders, funding payments, equity), not market \
                     data: not listed here, as `vike-cli data history ls` does not list it",
        })
    });
    json!(ListSeriesAnswer { withheld_account_series, ..bounded_series(series) })
}

/// The answer for `series`, cut at [`LIST_SERIES_LIMIT`] and saying so when it was.
pub(super) fn bounded_series(mut series: Vec<Value>) -> ListSeriesAnswer {
    let total = series.len();
    let cut = total > LIST_SERIES_LIMIT;
    series.truncate(LIST_SERIES_LIMIT);
    ListSeriesAnswer {
        series,
        truncated: cut.then_some(true),
        total: cut.then_some(total),
        note: cut.then(|| {
            format!(
                "TRUNCATED: the first {LIST_SERIES_LIMIT} of {total} series. This tool takes no filter; \
                 a series not shown may still be in the store, and the operator lists them all with \
                 `vike-cli data history ls`."
            )
        }),
        withheld_account_series: None,
    }
}

/// `list_series`' answer, its `outputSchema` too.
#[derive(Serialize, JsonSchema)]
pub(super) struct ListSeriesAnswer {
    /// each market series {kind, venue, symbol, group, interval, first_ts, last_ts, rows}
    series: Vec<Value>,
    /// present, and true, when `series` was cut short
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated: Option<bool>,
    /// with `truncated`: how many series the store holds
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
    /// with `truncated`: what was left out and how to list it all
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// how many ACCOUNT series (your own fills, orders, equity) were left out, and of which kinds
    #[serde(skip_serializing_if = "Option::is_none")]
    withheld_account_series: Option<Value>,
}

/// `delete_series`' refusal of a `kind` or `venue` it cannot read: absent, blank or no text.
fn required_refusal(name: &str) -> String {
    format!("`{name}` is required and must be a non-empty string")
}

/// `delete_series`' refusal of a `symbol`, `group` or `interval` it cannot read: blank or no text.
fn optional_refusal(name: &str) -> String {
    format!("`{name}` must be a non-empty string, or absent to wildcard that dimension")
}

/// `delete_series`' refusal of a `produced_by` it cannot read: absent, blank or no text.
const PRODUCED_BY_REQUIRED: &str = "`produced_by` is REQUIRED on this surface, for every call — a \
     sweep and a fully-named series alike. It is the commit-key PREFIX the rows carry, and every key \
     of every matched series must carry it or the whole run is refused. A model composes an identity \
     from context that may be stale; a provenance assertion is the one check that fails on a \
     plausible-but-wrong one. There is no override. `vike-cli data history rm --dry-run` reports each \
     series' keys, or ask the operator.";

/// Read a `delete_series` call's arguments into a [`DeleteIntent`] and the selector it names, refusing every
/// shape the store cannot act on.
///
/// ⚠ **`produced_by` is required HERE**, before anything is dialled and before a token is minted —
/// see [`Server::tool_delete_series`] for why it is unconditional on this surface and conditional on
/// the CLI. Every other rule is the ONE copy `vike-cli data history rm` and the datahub run too
/// (`vike_data::store::removal`): the kind by `StoreKindId`'s own `FromStr`, the selector's shape, account data,
/// and a glob or a producer PATH in `produced_by` (this tool deletes through a datahub, one perhaps too old to
/// resolve a path). What is deliberately NOT checked here is anything needing the store's layout table's rows (an
/// interval on a kind that does not sub-partition by one): that is the datahub's to judge.
fn delete_intent_from(args: &Value) -> Result<(DeleteIntent, SeriesSelector), String> {
    // A value of the wrong type is refused in the words its absence or blankness is.
    let args: DeleteSeriesArgs = parse_args(args, |name, _| match name {
        "kind" | "venue" => required_refusal(name),
        "produced_by" => PRODUCED_BY_REQUIRED.to_string(),
        _ => optional_refusal(name),
    })?;
    let required = |name: &str, value: Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| required_refusal(name))
    };
    let optional = |name: &str, value: Option<String>| match value {
        None => Ok(None),
        Some(s) if !s.trim().is_empty() => Ok(Some(s.trim().to_string())),
        // An EMPTY string is refused rather than read as "omitted": an empty `symbol=` is the
        // store's GROUPED-series sentinel, so an empty selector names neither layout. Omitting
        // the field is how a dimension is wildcarded.
        Some(_) => Err(optional_refusal(name)),
    };
    let kind = required("kind", args.kind)?;
    let venue = required("venue", args.venue)?;
    let symbol = optional("symbol", args.symbol)?;
    let group = optional("group", args.group)?;
    let interval = optional("interval", args.interval)?;
    let produced_by = args
        .produced_by
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(PRODUCED_BY_REQUIRED)?
        .to_string();
    let selector = SeriesSelector {
        kind: kind.parse().map_err(|e| format!("{e}"))?,
        venue: venue.clone(),
        symbol: symbol.clone(),
        group: group.clone(),
        interval: interval.clone(),
        // The tool has no lane argument: a model never reaches the recorder's tape.
        source: None,
    };
    selector.validate_shape().map_err(|refusal| refusal.rendered())?;
    if let Some(refusal) = account_data_refusal(selector.kind) {
        return Err(refusal.rendered());
    }
    // ⚠ A producer PATH is refused on this surface for a second reason beside the compatibility guard: an agent
    // composes an identity from context that may be stale, and a prefix read off a dry run's commit keys is
    // evidence from the store while a path is a guess about a source tree it cannot see.
    if let Some(refusal) = produced_by_refusal(&produced_by, true) {
        return Err(refusal.rendered());
    }
    Ok((DeleteIntent { kind, venue, symbol, group, interval, produced_by }, selector))
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
    plan: Option<&vike_data::store::removal::RemovalPlan>,
    plan_error: Option<&str>,
    reason: Option<&str>,
    preview_token: Option<&str>,
    refused: Option<&vike_data::store::removal::RemovalRefusal>,
) -> Value {
    let note = match (refused, plan) {
        (Some(refusal), _) => format!(
            "{} PREVIEW ONLY — nothing was deleted, and no preview_token was issued: the store's own dry run \
             refused the provenance assertion (`refused` names each series), so there is nothing to confirm. \
             Change `produced_by` or the selector, and plan again.",
            refusal.rendered()
        ),
        (None, Some(_)) => DELETE_PLANNED.to_string(),
        (None, None) => DELETE_NOT_PLANNED.to_string(),
    };
    let mut preview = json!({
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
        // ⚠ HONESTLY false, always: this tool touches no vike-tradehub, so there is no node
        // verdict behind it and never will be. The field is carried because it is the one an agent
        // is taught to read as "the verdict that counts", and its ABSENCE on one write tool would
        // be read as a verdict rather than as its absence. What plays that role here is
        // `provenance_satisfied`, which is the STORE's own answer.
        "verified_by_node": false,
        // A store partition is not a mount — see [`VENUE_CHECK_NONE`].
        "venue_check": VENUE_CHECK_NONE,
        "preview_token": preview_token,
        "reason": reason,
        "note": note,
    });
    // The verdict's lines, under the CLI's `--json` field name, only on a refused plan.
    if let (Some(lines), Some(fields)) =
        (plan.and_then(|p| p.verdict().err()), preview.as_object_mut())
    {
        fields.insert("refused".to_string(), json!(lines));
    }
    preview
}

/// The note of a plan the store answered and passed.
const DELETE_PLANNED: &str = "PREVIEW ONLY — nothing was deleted. `plan` is the SERVER's own dry run \
     against the store it has open: which series matched, what each holds, and the commit keys that \
     wrote them. Read `matched`/`rows`/`bytes` and compare them to what you expected BEFORE \
     confirming — a wrong selector usually shows up as a count you did not predict. To \
     execute, call again with BOTH \"confirm\": true AND this exact \"preview_token\". The \
     token fires once, expires after 60s, and is bound to THIS selector AND this \
     `produced_by` — confirming a different deletion with it is refused. Deletion is \
     IRREVERSIBLE, and a window a venue no longer serves cannot be re-fetched.";

/// The note of a plan the store could not be asked for.
const DELETE_NOT_PLANNED: &str = "PREVIEW ONLY — nothing was deleted. ⚠ THE STORE WAS NOT ASKED \
     (`plan_error` says why), so there is NO plan: `matched`, `rows` and `bytes` are null rather than \
     zero, and you have been shown nothing to confirm. Do not confirm this. A confirming call would \
     fail at the same connection, so nothing can be deleted through it — fix the connection, \
     take a fresh plan, and read it before you decide.";
