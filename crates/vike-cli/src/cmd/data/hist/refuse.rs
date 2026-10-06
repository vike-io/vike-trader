//! The refusals the `hist` verbs share: a flag on the wrong verb, a malformed window or spec, a kind.
use vike_model::{parse_date_label, time::epoch_ms_to_utc_date};

use super::{Sub, Window, universe};

/// ⚠ **`repair` has no REMOTE route, and `--addr` is refused rather than ignored.** The sentence,
/// and the argument behind it, live here because three separate facts each say no and a reader who
/// only meets one of them will try to add the route back.
///
/// 1. **The wire's own rule.** `docs/decisions/0057`'s decision 3 admits a write-shaped verb to the
///    Observe side only when it is COST-bounded by server constants, ADDITIVE, IDEMPOTENT and
///    CONTAINED, and operator-armed. A rebuild is none of the first four: its cost is however many
///    part footers the series has, it REPLACES an index rather than adding to one, and it clears a
///    delta log. `vike_datahub_client::proto`'s `FEATURE_DELETE_SERIES` is the precedent for the
///    Control side, and `docs/decisions/0050` is why a key-less datahub would serve no such verb
///    at all — so the remote route would exist for a minority of deployments.
/// 2. **The datahub's vocabulary cannot name the broken series.** Every store-metadata RPC starts
///    from `DataFusionHist::list_series`, which finds leaves by the presence of `_manifest.json` —
///    so the headline failure this verb repairs is absent from `inventory()` and from every answer
///    a datahub can give. A remote repair would be reachable for exactly the series that do not
///    need one.
/// 3. **It would be a SECOND writer on a hot series**, which is the reopening clause
///    `docs/decisions/0060` names by hand — and the process holding the store open over that socket
///    is usually the recorder itself.
///
/// So the route is `--store` (or the resolved default) and the engine, on the box the store is on,
/// which is also where somebody repairing a store already is.
pub(crate) fn refuse_the_remote_route_on_repair() -> String {
    "--addr asks a running vike-datahub about the store THAT process opened, and `repair` has no \
     remote route. Three reasons: a rebuild is neither cost-bounded nor additive nor idempotent, \
     so it does not meet the bar a write-shaped wire verb has to clear; a datahub can only name \
     series it ENUMERATED, and a series whose base manifest is missing is in no enumeration — \
     which is exactly the series this repairs; and the process serving that socket is usually the \
     writer a rebuild must not collide with. Run it on the box, with --store DIR (or none, for the \
     resolved default)."
        .to_string()
}

/// Refuse a fetch WINDOW on a subcommand whose span is fixed — `seed-demo`'s synthetic curve and
/// `fetch-starter`'s published dataset.
///
/// Refused rather than ignored, for one reason both share: a `--days 30` that quietly did nothing
/// would leave an operator believing they had a month of history that is not there.
pub(super) fn refuse_a_window(
    sub: Sub,
    days: &Option<String>,
    from: &Option<String>,
    to: &Option<String>,
    why: &str,
) -> Result<(), String> {
    for (flag, present) in
        [("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())]
    {
        if present {
            return Err(format!(
                "{flag} bounds a `fetch` window and does not apply to `{}` — {why}",
                sub.as_str()
            ));
        }
    }
    Ok(())
}

/// ⚠ **A BLANK `--produced-by` is refused, and this is the sentence that says why.**
///
/// The words are `vike_data::store::store_kind::resolve_produced_by`'s own, deliberately: that resolver is
/// the ENGINE's one site for this rule, this crate cannot link it (`vike-data` is a DEV-dependency
/// here — the module doc's note on `SeriesRow` carries the edge), and the two must not answer
/// differently about the same token on the same verb.
///
/// **What a blank prefix actually does is the opposite of what it looks like.** It does not match
/// nothing — `vike_data::store::store_kind::key_matches_prefix` is `starts_with`, so EVERY commit key
/// satisfies it, `vike_data::store::removal::RemovalPlan::verdict` finds no foreign key in any series, and
/// the whole provenance assertion passes VACUOUSLY. And because the value is `Some` rather than
/// `None`, the rule that REQUIRES an assertion before a wildcard delete sees a value and stands
/// down. Two guards fall to one blank token, on an IRREVERSIBLE verb.
///
/// ⚠ **It is reachable from a script, not only from a typo**: `--produced-by="$PREFIX"` with
/// `PREFIX` unset collapses to `--produced-by=`, and `--produced-by "$PREFIX"` to `--produced-by
/// ""`. `crate::cmd::args`'s `Flags::value` accepts both — an empty string is not a flag token —
/// so both reach here.
///
/// ⚠ **It was already refused, and that is the interesting part.** The refusal was a row in the
/// SELECTOR loop above, whose message argues about the store's grouped-series `symbol=` sentinel
/// and tells the operator to *omit the flag to wildcard the dimension* — which for this flag means
/// turn the assertion off, i.e. the one thing a sweep refuses. So the only thing standing between
/// a blank prefix and the wire was an untested line that did not know what it was guarding, and
/// the refactor ruling 12 asks for is exactly the edit that drops it. Hence a function, hence its
/// own tests on BOTH routes.
///
/// ⚠ **On the remote route there used to be nothing behind it, and there is now.** Until
/// 2026-09-11 `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb` tested
/// `produced_by.is_none()` for the sweep gate — which `Some("")` satisfies — and handed the raw
/// spelling to `plan_removal`, so a blank prefix getting past this check was a wildcard delete
/// wearing an assertion. That server now resolves the spelling through
/// `vike_datahub_client::proto`'s `resolve_produced_by` at its own door.
///
/// **This refusal stays, and it is not redundant.** It refuses BEFORE a socket is dialled, which is
/// strictly better than a round trip; it is the only guard on the LOCAL route's argv before the
/// engine is spawned; and a `vike-cli` this new will meet datahubs older than the fix for as long
/// as one is deployed — the protocol is capability-negotiated, and there is no capability string
/// for "this server validates its provenance filter".
/// Refuse a cleanup aimed at ACCOUNT data, by name.
///
/// `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 3 requires
/// this, and the reason is narrower and sharper than "wrong plane". `kind=exec_funding` partitions
/// on `(venue, coin)` while its commit key carries `{account}` — so two accounts' funding payments
/// for one coin land in ONE series, and `rm` deletes BY SERIES. An `rm` aimed at a test account's
/// history takes the live account's with it, silently. There is no selector that could scope it,
/// because the dimension the operator would need is not in the partition.
///
/// So this plane does not offer the deletion at all, rather than offering one that cannot be
/// aimed. The same refusal covers the other three account kinds for the plainer reason in the
/// surface design's §9.3.2: they are not market data and belong to a future `vike-cli account`.
///
/// ⚠ **The roster is NOT a copy.** `vike_model::is_account_kind` is the one declaration, and
/// `crates/vike-ops/tests/venues/store_kind_gate.rs` holds the store's own rows against it. An earlier
/// version of this function carried a private `ACCOUNT_KINDS` array held equal by a text gate that
/// parsed both source files; the root `CLAUDE.md` names the better cure — *a shared crate BELOW
/// both* — and `vike-model` is already a normal dependency of this crate and of `vike-data`, with
/// no DataFusion in it. The copy and its gate are gone.
pub(super) fn refuse_an_account_kind(kind: &str) -> Result<(), String> {
    if !vike_model::is_account_kind(kind) {
        return Ok(());
    }
    let extra = if kind == "exec_funding" {
        " — and for this kind a scoped delete is not merely unimplemented but UNEXPRESSIBLE: the \
         series partitions on (venue, coin) while the account rides in the commit key, so one \
         series holds every account's payments for that coin and `rm` deletes by series"
    } else {
        ""
    };
    Err(format!(
        "`{kind}` is ACCOUNT data, not market data, and `data hist rm` refuses it{extra}. Your \
         fills, orders, funding payments and equity belong to `vike-cli account`, which is not \
         built yet. The market funding RATE is unaffected and is not an account kind — it is \
         `--kind bar --interval funding`."
    ))
}

pub(super) fn refuse_a_blank_produced_by(produced_by: &str) -> Result<(), String> {
    if produced_by.trim().is_empty() {
        return Err(format!(
            "--produced-by {produced_by:?} is BLANK, so there is nothing to assert against — an \
             empty prefix matches every key, which makes the provenance check pass for every \
             series while looking like an assertion, and satisfies the rule that REQUIRES one \
             before a wildcard delete. Pass a literal prefix instead, or omit the flag."
        ));
    }
    Ok(())
}

/// ⚠ **A PRODUCER PATH is refused on the REMOTE route — a COMPATIBILITY guard since 2026-09-11,
/// and a stand-in for a missing check before that.**
///
/// `--produced-by` has two spellings: a literal commit-key prefix (`panel_bars:`), and the
/// repo-relative path of a declared producer, which `vike_data::store::store_kind::resolve_produced_by`
/// turns INTO its prefix by reading `STORE_KINDS`. That resolver used to have exactly one caller in
/// the tree — `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series`, i.e. the LOCAL route —
/// while `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb` called neither it nor anything
/// like it: it handed the raw spelling to `plan_removal`.
///
/// So the same command line answered two ways. `--produced-by crates/vike-data/src/demo.rs` with
/// `--store` resolved to `demo-tape:v`, the assertion held and the series were deleted; the same
/// line with `--addr` sent the PATH as a literal prefix, `key_matches_prefix` (`starts_with`)
/// matched no key in any series, `RemovalPlan::verdict` called every key foreign, and the operator
/// was told their store had foreign provenance — when in fact their flag had never been resolved.
/// It fails SAFE (a path no key starts with can only refuse), and that is exactly what made it
/// survive: the wrong answer looks like a serious finding about the data rather than a defect.
///
/// THREE surfaces promised the resolution unconditionally — this module's `USAGE`, `RmArgs`'s
/// `produced_by` doc, and `crate::cmd::mcp`'s `delete_series` tool schema, which is REMOTE-ONLY and
/// so was never true. All three now say where it holds; this refusal is what makes the boundary
/// visible at the moment it is crossed instead of a page later.
///
/// ⚠ **THE ASYMMETRY IS CLOSED ON THE SERVER, and this refusal survives as a compatibility guard
/// rather than as a stand-in.** The follow-up this doc named — a `resolve_produced_by` re-export in
/// `vike_datahub_client::proto`, beside the `SeriesSelector`/`describe_id` re-exports that exist for
/// exactly this reason — landed with the wire's blank-filter fix, and `delete_series_verb` now
/// resolves a path exactly as the local route does. What it does NOT close is the mixed fleet: this
/// protocol is capability-negotiated rather than version-gated and carries no capability string for
/// "this server resolves producer paths", so a new CLI cannot tell a fixed datahub from an
/// unfixed one and must keep refusing to send a spelling the older one would assert literally.
/// Deleting this refusal hands #1754's defect back to every operator pointing a current `vike-cli`
/// at a datahub that has not been redeployed.
pub(super) fn refuse_a_producer_path_on_the_remote_route(produced_by: &str) -> Result<(), String> {
    if produced_by.contains('/') {
        return Err(format!(
            "--produced-by {produced_by:?} looks like a PRODUCER PATH, and a datahub that has not \
             been redeployed since 2026-09-11 resolves none — it would be asserted as a literal \
             prefix, match no commit key in any series, and report your data as foreign when in \
             fact the flag was never resolved. This protocol carries no capability string for \
             \"this server resolves producer paths\", so a fixed datahub and an unfixed one cannot \
             be told apart from here and a path is refused before anything is dialled. Pass the \
             commit-key PREFIX literally (e.g. `panel_bars:`), or run the delete against a local \
             store (`--store DIR`), which always resolves a path."
        ));
    }
    Ok(())
}

/// Refuse the first flag in `present` that was actually given, naming the subcommand it was given
/// to and WHY it belongs elsewhere.
///
/// ⚠ The `why` sentence is the product, not the refusal. Every flag this function guards is one an
/// operator typed because a SIBLING subcommand accepts it, so "unknown option" would be a lie and
/// a bare "not allowed here" would leave them guessing which sibling. The messages therefore name
/// the store the flag would have reached and the flag that reaches the other one.
pub fn refuse_foreign_flags(sub: Sub, present: &[(&str, bool)], why: &str) -> Result<(), String> {
    for (flag, given) in present {
        if *given {
            return Err(format!("{flag} does not apply to `{}` — {why}", sub.as_str()));
        }
    }
    Ok(())
}

/// `--store` on a verb that READS history — `export` and every [`Sub::is_read`] verb — refused with
/// `vike_datahub_client::flag_vocab::store_flag_removed`, the ONE sentence every history reader in
/// this workspace prints, naming the verb as the operator typed it.
///
/// ⚠ **The read verbs used to answer this mistake with a sentence of their own** (*"… reach it
/// with --addr"*), and the change that closed `export`'s local read on 2026-09-26 first extended
/// that sentence by hand-spelling the key-less datahub command a second time — so
/// `data hist ls --store X` and `data hist export --store X` answered the same mistake two ways,
/// and the copy could drift from the shared one without any test noticing. Both go through the
/// shared sentence now.
///
/// ⚠ **A read verb ADDS one clause the shared sentence cannot carry**: its `--addr` reaches a
/// datahub that already has a store open on another box, which is the read an operator who typed a
/// server's store path usually wanted. `export` does not get the clause, deliberately — its
/// `--addr` does not name another source for the same file, it selects a different ROUTE that
/// writes rows rather than Parquet (see [`export`]'s module doc), so pointing at it from here would
/// answer a question about WHERE with a change of WHAT.
pub(crate) fn store_refusal(sub: Sub) -> String {
    let sentence =
        vike_datahub_client::flag_vocab::store_flag_removed(&format!("data hist {}", sub.as_str()));
    if sub.is_read() {
        format!(
            "{sentence} A store a datahub on another box already has open is read with \
             --addr HOST:PORT."
        )
    } else {
        sentence
    }
}

/// The spec's SHAPE: three non-empty `:`-separated parts. See this module's doc for why the venue
/// and interval themselves are the ENGINE's to judge.
pub(super) fn check_spec(spec: &str) -> Result<(), String> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "'{spec}' is not VENUE:SYMBOL:INTERVAL — three non-empty parts, e.g. binance:BTCUSDT:1h"
        ));
    }
    Ok(())
}

/// The window, as exactly one of the two forms.
///
/// ⚠ Mixing them is an error rather than a precedence rule. `--days 30 --from 2026-01-01` has
/// two readable meanings and no obviously right one, and whichever a precedence rule picked would
/// silently discard the other half of what the operator typed.
pub(super) fn window_from(
    days: Option<String>,
    from: Option<String>,
    to: Option<String>,
) -> Result<Window, String> {
    match (days, from, to) {
        (Some(d), None, None) => {
            let n: u32 = d
                .trim()
                .parse()
                .map_err(|_| format!("--days takes a whole number of days, got {d:?}"))?;
            if n == 0 {
                return Err("--days 0 covers no time at all".to_string());
            }
            Ok(Window::Days(d))
        }
        (None, Some(f), Some(t)) => Ok(Window::Range { from: f, to: t }),
        (None, Some(_), None) => Err("--from needs a matching --to".to_string()),
        (None, None, Some(_)) => Err("--to needs a matching --from".to_string()),
        (None, None, None) => Err(
            "fetch needs a window: --days N, or --from LABEL --to LABEL (epoch-ms, or a UTC date \
             YYYY-MM-DD such as 2024-01-01)"
                .to_string(),
        ),
        (Some(_), _, _) => {
            Err("--days and --from/--to are two ways to say the same thing — pass one".to_string())
        }
    }
}

/// `universe`'s window: BOTH bounds optional, BOTH independent, and BOTH parsed here.
///
/// ⚠ **The parse is the difference from [`ExportRange`], and it is the reason this verb has a
/// helper of its own.** An ENGINE export's bounds are forwarded to the engine as TEXT, and the
/// engine parses them its own way. Nothing is forwarded here: `universe` compares timestamps a
/// datahub already sent, in this process, so an unreadable bound has to be refused HERE or it would
/// be silently discarded into a window that means something else. It goes through
/// [`vike_model::parse_date_label`] — the SAME parser `fetch`'s bounds ([`fetch_window_ms`]) and a
/// remote export's go through, so those verbs cannot disagree about what `2026-01-01` means.
///
/// ⚠ This said "the SAME parser the engine's own bounds reach", and the engine's do not: an ENGINE
/// export hands its bounds to `crates/vike-backtest/src/harness/profile.rs`'s `parse_ts`, which
/// takes epoch-ms or an hour label (`2026-01-01T00`) and refuses a bare date — the one
/// spelling `parse_date_label` takes and the hour label the one it refuses. [`USAGE`]'s `--from`
/// row states both, by which process parses.
///
/// The four combinations are all well-formed (see [`ExportRange`] for that argument), so the only
/// refusals are an unreadable label and an INVERTED pair — and the second is refused rather than
/// swapped, for [`window_from`]'s reason: a range whose ends are the wrong way round has two
/// readable meanings, and picking one discards half of what the operator typed. An inverted window
/// would otherwise report every instrument in the store `absent`, which reads exactly like an
/// empty store.
pub(super) fn membership_window(
    from: Option<&str>,
    to: Option<&str>,
) -> Result<universe::MembershipWindow, String> {
    fn bound(flag: &str, raw: Option<&str>) -> Result<Option<i64>, String> {
        let Some(raw) = raw else { return Ok(None) };
        match parse_date_label(raw) {
            Ok(ms) => Ok(Some(ms)),
            Err(e) => Err(format!(
                "{flag} {raw:?} is not a timestamp this side can read ({e}). `universe` compares \
                 dates in THIS process rather than forwarding them, so the bound is parsed here: \
                 epoch-ms, or YYYY-MM-DD"
            )),
        }
    }
    let from_ms = bound("--from", from)?;
    let to_ms = bound("--to", to)?;
    match (from_ms, to_ms) {
        (Some(f), Some(t)) if f > t => Err(format!(
            "--from ({}) is AFTER --to ({}) — an inverted window contains nothing, so every \
             instrument in the store would be reported `absent`, which is indistinguishable from \
             an empty store. Pass them the other way round.",
            epoch_ms_to_utc_date(f),
            epoch_ms_to_utc_date(t)
        )),
        _ => Ok(universe::MembershipWindow { from: from_ms, to: to_ms }),
    }
}
