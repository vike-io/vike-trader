//! Halt-admit reporting and the paper-fallback path: what happens when a venue can't (or won't)
//! go live — degrade to `PaperExecutionClient`, report why, and warn once if the operator's
//! credentials say a venue should be live but `policy.toml` hasn't caught up.

use std::collections::HashMap;

use vike_model::account_keys::AccountLabel;

use crate::{
    EngineAndRecon, MountPolicy, arm_universal_defaults, margin_mode_grid, merge_operator_budget,
    multiplier_grid, resolve_fee_schedule, symbol_grid, would_mount_live_under,
};

/// **Whether `venue`'s stored credentials would arm it under EITHER tier** — `demo` or `live` —
/// which is the true question both reports below ask ("does this venue have credentials being
/// wasted under `paper`", "which venues would a `[venues]` table have to name"). It is NOT
/// `crate::would_mount_live`: that probes ONLY the `Live` ceiling, and decision 0095 broke the
/// assumption that a `Live` probe subsumes what a `Demo` probe would find — for
/// binance/bybit/okx/hyperliquid a `live` ceiling with only DEMO keys is now PAPER (it no longer
/// falls back to the demo tier), so `would_mount_live` alone silently drops every
/// demo-only-credentialed row of those four venues from a report whose whole job is to name every
/// venue with SOME credentials.
fn would_mount_under_some_tier(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
) -> bool {
    would_mount_live_under(registry, venue, vars, vike_config::VenueMode::Live)
        || would_mount_live_under(registry, venue, vars, vike_config::VenueMode::Demo)
}

/// Resolve the halt-admit mode in force at `venue`, and SAY SO when the venue cannot honour what was
/// asked for.
///
/// ⚠ **The report is the feature, not the resolution.** An operator who runs
/// `vike-cli config set policy.halt_admit verify` believes their kill switch now verifies positions
/// before letting anything out. On every roster venue but cTrader it cannot — no position book
/// exists at that boundary — and a SILENT degrade would leave them believing it right up until an
/// incident. So the line lands at MOUNT, with the venue and the reason, exactly as
/// `vike_bridge_core::halt::halt_path_arming_error`'s probe reports an unarmable sentinel path at
/// mount rather than at the first order. Same lesson, paid for once already.
///
/// ⚠ **This half — and only this half — can be answered up front.** "This venue's adapter holds no
/// position book" is a fact about the CODE, true before a credential is read. Whether `verify`
/// actually ARMED is a fact about the MOUNT, and is not known here:
/// [`crate::make_engine`] calls this before credentials and before cTrader's blocking handshake,
/// either of which can land the venue on the paper client, which has no position book either.
/// [`report_halt_admit_armed`] is that second half, said where the outcome is known.
///
/// Silent under the DEFAULT: `HaltAdmit::Admit` degrades nowhere, so a machine with no `policy.toml`
/// gains no log line at all — the byte-identical claim holds in the trace file too, not just on the
/// wire.
pub(crate) fn report_halt_admit(
    venue: &str,
    policy: Option<&MountPolicy>,
) -> vike_model::HaltAdmit {
    let requested = policy.map(|p| p.halt_admit).unwrap_or_default();
    let (effective, degraded) = vike_model::effective_halt_admit(requested, venue);
    if let Some(why) = degraded {
        tracing::warn!(
            venue,
            requested = %requested.as_str(),
            effective = %effective.as_str(),
            reason = %why,
            "`policy.halt_admit` is `verify`, but this venue cannot honour it → DEGRADING \
             to admit (the HALT sentinel still stops opening orders here; it just cannot check a \
             reduce_only claim against a position). See docs/ops/kill-switches.md"
        );
    }
    effective
}

/// Say whether `verify` actually ARMED, from the one place that knows: after the venue's mount has
/// resolved to a LIVE client or fallen back to paper.
///
/// ⚠ **Announcing this from [`report_halt_admit`] was a claim the mount could not keep.** That
/// function runs at the TOP of [`crate::make_engine`] — before credentials are loaded, and before
/// cTrader's synchronous `connect_and_auth_exec`, which on failure demotes the venue to
/// `vike_paper::PaperExecutionClient` for the session. Both paths produce a client with no position
/// book, so an `info!` up there told an operator that `verify` was armed on a venue that had mounted
/// PAPER. Under `verify` a paper mount is not merely un-tightened, it is the exact shape of the
/// original defect: the operator believes a check is running that is not.
///
/// So the NOT-armed case is a `warn!` rather than silence. `verify` on a paper venue is a
/// misconfiguration an operator wants to see — either the credentials are missing or the handshake
/// failed, and both are things they came to the log to find out.
///
/// Nothing is logged under `HaltAdmit::Admit`, on any path: the default must add no line anywhere.
///
/// The DECISION is `vike_model::halt_admit_arming`, a pure function with its own exhaustive test —
/// this is only the `tracing` shell over it, for the same reason `effective_halt_admit` lives in
/// vike-model and logs nothing. Asserting on a log line needs a subscriber harness; asserting which
/// of three answers a (mode, outcome) pair produces does not.
pub(crate) fn report_halt_admit_armed(venue: &str, effective: vike_model::HaltAdmit, live: bool) {
    match vike_model::halt_admit_arming(effective, live) {
        vike_model::HaltAdmitArming::Silent => {}
        vike_model::HaltAdmitArming::Armed => tracing::info!(
            venue,
            "halt_admit=verify is ARMED on this venue: under an engaged HALT a reduce_only submit \
             is checked against the venue's own position book, and refused only when that book \
             PROVES it opens risk (an unknown book still admits — a halt must never trap you)"
        ),
        vike_model::HaltAdmitArming::MountedPaper => tracing::warn!(
            venue,
            "`policy.halt_admit` is `verify` and this venue's adapter CAN honour it, but \
             it mounted PAPER (no credentials, or the exec handshake failed) — the paper client \
             holds no position book, so nothing is verified this session. See \
             docs/ops/kill-switches.md"
        ),
    }
}

/// One execution engine for `venue`/`symbol`, type-erased as `Box<dyn ExecutionClient>` so the
/// cross-venue core can drive Binance/Bybit/OKX engines together. ALSO returns an
/// `Option<Box<dyn ReconClient>>` for the same venue/symbol (audit A1 item 4) — `Some` for a live
/// (credentialed) venue whose bridge's mount built one (every reconciled venue's client is built
/// there since docs/decisions/0096, e.g. `crates/bridges/binance/src/mount.rs`'s
/// `BinanceVenueMount`); `None` for a paper venue. Consumed by the `recon_driver` `spawn_recon`
/// mount `vike_mount::build_node`'s caller makes from `Node::recon_clients`, gated on that root's
/// resolved reconcile verdict (`vike_tradehub::reconcile_config::reconcile_gate` — ON by default
/// for a live mount since S2), so building it here changes nothing about the engine itself.
///
/// LIVE GATE: each venue's bridge decides, in its own `resolve` and `mount`, from the credential
/// store it is handed (`MountInputs::secrets`) and the ceiling (`MountInputs::live_permitted`):
/// binance/bybit/okx read their `{VENUE}_DEMO_*` keys under a `demo` ceiling and their
/// `{VENUE}_LIVE_*` keys under a `live` one (decision 0095), and deribit its demo keys under
/// either; absent creds → the PAPER client (absent-credentials-is-the-live-gate). Binance routes
/// spot for a plain symbol (`BTCUSDT`) and the USDⓈ-M perp for a `.P` symbol (`BTCUSDT.P`).
/// ⚠ Aster is the REAL-MONEY exception: it has NO `{VENUE}_DEMO_*` shape — its mount
/// (`AsterVenueMount` in `crates/bridges/aster/src/mount.rs`) resolves agent-wallet creds directly,
/// preferring LIVE (mainnet) then TESTNET, so present `ASTER_LIVE_*` keys under a `live` ceiling
/// spawn a LIVE MAINNET exec client (real money). ⚠ Polymarket is the SECOND
/// real-money exception and the only venue with no testnet at all, so it needs `flags.poly_exec` ON TOP
/// of its credentials before any exec client is built; without that flag it stays paper exactly as
/// before. A live venue is recorded in `live_venues` so the DOM lights
/// its `● LIVE` badge and the window title reads LIVE. The PAPER client fills against that venue's
/// live 1m bars via the core's `on_bar` seam (so its bar feed must be subscribed — see
/// `ensure_depth`); a LIVE client ignores `on_bar` (fills arrive on its user-data WS).
///
/// `recon_enabled` is the GLOBAL reconciliation on/off — the caller's already-resolved verdict from
/// `vike_tradehub::reconcile_config::reconcile_gate` (`vike_mount::NodeConfig::recon_enabled`) — and it exists SEPARATELY
/// from `recon_trigger` below on purpose, because the obvious `recon_trigger.is_some()` shortcut is
/// WRONG. That trigger is a per-venue RECONNECT poke, and it is wired for exactly four venues
/// (binance/bybit/okx/hyperliquid); `vike_mount::build_node` passes a hardcoded `None` for
/// deribit/aster/alpaca/ctrader/ig/oanda/ibkr/polymarket EVEN WHEN reconciliation is fully on, since
/// those venues reconcile on the periodic interval only. Reading the trigger as the global gate
/// would therefore silently stop reconciling six live venues.
///
/// What it GATES is the five venues whose `ReconClient` is built with a BLOCKING, AUTHENTICATED
/// handshake at mount time — deribit (a dedicated authed order-WS, built inside its bridge's own
/// mount, which is handed this gate as `MountRequest::recon_enabled`), ctrader (a
/// protobuf/TLS OAuth socket), ig (a full `IgSession` login), oanda (a Bearer `OandaRest` +
/// `/summary` probe) and, behind the `ibkr` feature, ibkr (a cpapi `tickle`/`secdef_search`). Those
/// were constructed UNCONDITIONALLY, so a `VIKE_RECONCILE`-unset mount still did authenticated
/// network work against five venues and then threw the handle away. `false` (reconcile off) now
/// means the factory is never CALLED at all — see `vike_bridge_core::venue_mount::recon_if_enabled`,
/// which is what makes that laziness testable — and `recon` stays `None`, exactly as for a paper
/// venue. `true` is
/// byte-identical to the pre-gate behavior: same factory, same arguments, same ordering, same
/// `None`-on-failure handling.
///
/// Deliberately NOT gated (each already correct, or not a mount-time network call): the crypto-CEX
/// and aster bridges' `recon_client` factories (each called from its bridge's `VenueMount::mount`,
/// docs/decisions/0096 — aster's was the last inline factory in this crate) construct no
/// connection (alpaca's is called by `AlpacaVenueMount::mount`, which ignores
/// `MountRequest::recon_enabled`); hyperliquid's recon client is inseparable from the exec client
/// it is built with.
///
/// ⚠ **Polymarket was the fourth name in that sentence and did not belong there.** It read
/// "polymarket has its own equivalent inner gate (`POLY_RECONCILE=1`, checked BEFORE its factory)",
/// and the inner gate is not equivalent: it decides whether the VENUE wants a client, not whether
/// this process mounts the driver that would use one, so with `flags.poly_reconcile` on and reconciliation
/// off the arm still paid a blocking authenticated L1→EOA + `/auth/derive-api-key` round trip and
/// then had the handle dropped by a driver-less root — the build-then-discard defect this parameter
/// exists to have removed, one venue further on. That arm — its bridge's mount since the venue
/// mount contract — now reads BOTH (`crates/bridges/polymarket/src/exec_plane/mount.rs`'s
/// `poly_recon_wanted`), so `false` here means no Polymarket network work either.
///
/// `recon_trigger` (reconciliation-activation Task 7) reaches each venue whose declaration's
/// `takes_recon_trigger` is `true` — binance, bybit, okx and hyperliquid — through the contract
/// fold, and each of those bridges' mounts threads it into its venue's resync path. It is built
/// ONCE by the caller (`vike_mount::build_node`, BEFORE any `make_engine` call — the `ReconDriver`
/// itself doesn't exist yet at this point, see `spawn_recon`'s doc for why) and cloned per venue,
/// so a reconnect on ANY of them pokes the SAME shared reconcile driver.
/// Deribit's mount never receives this parameter — its declaration's `takes_recon_trigger` is
/// `false`, so the contract fold hands it `None` — and callers pass `None` for it regardless of
/// whether recon is enabled overall. `None` for a
/// paper venue is inert (the paper client never touches `on_reconcile`); `None` everywhere (recon
/// disabled) reproduces the pre-Task-7 behavior byte-for-byte.
///
/// `properties_rec` is the caller-CONSTRUCTED opt-in PIT-filter recorder
/// (`vike_data::PropertiesRecorder::open_from_env` over the binary's tick-store root — `None`
/// unless `VIKE_RECORD_PROPERTIES == "1"`): the five recording mounts (the
/// binance/bybit/okx/aster/deribit bridge mounts) move it into their venue `spawn_with_recorder`
/// calls so the fetched `SymbolProperties` grid is recorded at instrument-fetch time. Constructed
/// by the BINARY rather than here because the constructor names the concrete `DataFusionHist`
/// backend (gated on vike-data's `hist-datafusion` feature, which this crate deliberately no longer
/// enables — the DataFusion-edge cut): a binary built without that backend passes `None`,
/// byte-identical to the recorder's own disabled path. Callers that mount several venues clone the
/// SAME `Arc` handle per call, so all recording mounts share one store handle (previously each arm
/// opened its own — safe either way, per-series locks).
///
/// `risk_profile` (RunProfile wiring — closing the live gap) is an optional OPERATOR-owned `[risk]`
/// budget applied to EVERY venue this function mounts, merged onto the venue-fetched grid AFTER
/// every arm above has run but BEFORE the `im_requirement` rescue below (that ordering is
/// load-bearing — see that merge site's comment for why). `None` — the value every call site
/// passed before this parameter existed — leaves the PROFILE contribution untouched, but as of
/// Task 6 (armed-risk-defaults) the mount is no longer byte-identical to before that task on this
/// account: `max_orders_per_window`/`window_ms` now ALWAYS arm to a conservative default (see
/// [`arm_universal_defaults`]) regardless of `risk_profile`, exactly like the pre-existing
/// `im_requirement` rescue directly below. (`max_leverage` was armed there too until issue #822
/// removed it: it enforced nothing, and the leverage floor it duplicated is the `im_requirement`
/// rescue — which an operator's `[risk] max_leverage` now converts into.) `Some` overrides those defaults with the
/// profile's own values (tuning, not fighting — see `ProfileRisk::apply_to`'s doc for the merge
/// rule); the venue still owns the instrument grid — see the application site's comment for what
/// happens if a profile illegally sets one of those fields anyway (an `Err` there never drops the
/// WHOLE operator budget — only the offending venue-owned fields, via
/// `ProfileRisk::apply_operator_budget_only`). `max_notional_per_order`/`max_total_exposure` are
/// the OTHER kind (account-dependent, no universal safe default) — see [`require_live_risk_budget`]:
/// a LIVE mount (this venue actually resolved credentials) with neither set from any source
/// returns `Err(MountError::MissingRiskBudget)` instead of an engine; a paper/backtest mount is
/// unaffected and may run with both unbounded.
///
/// **Caller contract:** this function has no way to see a `RunProfile`'s `mode` — it only ever
/// receives the bare `ProfileRisk` table — so a caller resolving one from a full `RunProfile` MUST
/// gate it through `vike_core::RunProfile::risk_for_live_venue_mount` first (`Err` unless
/// `mode == Mode::Live`) rather than reading `.risk` directly. A `backtest`/`paper` profile may
/// LEGALLY set the venue-owned instrument fields (its own `mode` implies
/// `GridSource::NoGridFetched`), which would make EVERY venue arm's merge below fail under this
/// function's hardcoded `GridSource::VenueFetched` — the per-venue fallback keeps the mount armed,
/// but a profile that was never meant to reach a live mount should fail loud at resolution, not
/// merely degrade quietly here.
///
/// `policy` (settings-unification Phase 6c) is this MACHINE's hard ceilings — the deployment's
/// `<vike home>/policy.toml`, loaded by the BINARY (`vike_config::load` takes the environment as a
/// MAP; only binaries read `std::env`) and projected onto [`MountPolicy`], the subset this function
/// actually applies. It is a DIFFERENT authority from `risk_profile`: a run profile is per-RUN and
/// operator-owned, a policy is per-MACHINE and admin-owned with no env or CLI layer at all.
///
/// ⚠ **`None` IS NO LONGER BYTE-IDENTICAL, and that is this stage's whole subject.**
/// `MountPolicy::venues` — the per-venue ARMING CEILING — defaults to `paper` for every venue, and
/// a `None` policy reads the same way, so a caller that threads no policy mounts every venue PAPER
/// regardless of what its credentials say. The direction is the safe one by construction
/// (`VenueMode::cap` is `min`) and it is deliberately the fail-safe end: a ceiling whose absent
/// value armed everything would not be a ceiling, and the defect being closed is precisely that
/// credential presence was the only gate (MEASURED on the CI box: a one-venue run profile holding NINE
/// live authenticated exec sessions). The refusal happens ABOVE the credential read — a capped
/// venue loads no credential, fetches no instrument grid and opens no socket — and it announces
/// itself twice: once per venue ([`report_capped_to_paper`]) and once per process, with a
/// paste-ready block, for a box that has credentials and no `[venues]` table
/// ([`venue_arming_migration`]).
///
/// For every OTHER field, `None` — every call site before this parameter existed, and every
/// deployment with no `policy.toml` — is BYTE-IDENTICAL to that mount: `MountPolicy::default()` is
/// the no-file value and each venue keeps its own compiled-in behaviour. The fields that bind
/// something here are `market_slippage`, consumed by the hyperliquid bridge's mount (the only
/// roster venue with no native market order, so one venue IS full coverage rather than partial
/// wiring), and `halt_admit`, consumed by cTrader's mount (`crates/bridges/ctrader/src/mount.rs`'s
/// `CtraderVenueMount`) and REPORTED for every venue — the degrade where the venue cannot honour
/// `verify` ([`report_halt_admit`], at the top of this function), and whether it actually ARMED
/// where it can ([`report_halt_admit_armed`], once the mount's outcome is known, because that is
/// the first point where a paper fallback is ruled out). ⚠ No count is written here: the
/// sentence this replaced said "today exactly one field binds anything" and went stale the first
/// time a second one did.
/// `MountPolicy`'s own module doc names every `Policy` field it deliberately does NOT carry, with
/// the reason; that projection is gated by an exhaustive destructure, so a new ceiling cannot be
/// added to `Policy` and silently ignored down here.
// 10 params (was 9 before `policy`, 8 before `recon_enabled`, 7 before `risk_profile`): this is the
// venue-assembly seam, and every argument is a distinct axis the 12 venue arms select on. Bundling
// them into a config struct would touch ~19 call sites for no readability gain — revisit if it
// grows again.
/// The paper exchange EVERY venue arm falls back to, built ONCE here so it is armed ONCE here.
///
/// Each of the eleven paper fallbacks in [`make_engine`] used to spell
/// `vike_paper::PaperExecutionClient::with_fee_schedule(venue, symbol, 0.0002, static_default)`
/// verbatim. That was harmless while a paper book had no behaviour to configure; it stopped being
/// harmless the moment one did, because "add the new call to all eleven" is a step a twelfth venue
/// arm silently skips — and the thing being skipped is the operator kill switch.
///
/// ⚠ **The arming is the reason this function exists.** A mounted paper book is still a MOUNT: it is
/// what a daemon runs when a venue has no credentials (the live gate), what `vike-tradehub`'s paper
/// variant runs end to end, and therefore where an operator REHEARSES `touch <project>/settings/state/HALT`.
/// Before this, that `touch` did nothing on a paper mount, silently — see
/// `crates/vike-paper/src/lib.rs`'s `with_halt_path` for why the paper client takes the sentinel as
/// a parameter instead of resolving one itself, and why a BACKTEST must never get one.
///
/// The path is `crates/vike-bridge-core/src/halt.rs`'s `halt_path_from_env` — the same
/// once-per-process resolution (and once-per-process armability report) every venue adapter on the
/// shared `ExecActor` consults, so a mixed live/paper node has exactly ONE sentinel to `touch`.
pub(crate) fn paper_client(
    venue: &str,
    symbol: &str,
    fee_schedule: vike_model::FeeSchedule,
) -> vike_paper::PaperExecutionClient {
    vike_paper::PaperExecutionClient::with_fee_schedule(venue, symbol, 0.0002, fee_schedule)
        .with_halt_path(vike_bridge_core::halt::halt_path_from_env())
}

/// The whole engine a venue gets when the ARMING CEILING refused it — assembled WITHOUT reading a
/// credential, fetching an instrument grid or opening a socket.
///
/// It is a separate assembly rather than a fall-through because the point of the refusal is that
/// nothing below it runs: a capped venue that reached [`make_engine_with_legs`]'s match would still
/// have resolved credentials on its way there, which is the exposure being closed. What it must NOT
/// be is a DIFFERENT engine from the one an uncredentialled venue gets — the ceiling refuses an
/// arming, it does not invent a new mode — so the duplication is guarded by an output comparison
/// rather than by a comment: `a_capped_venue_is_the_same_engine_an_uncredentialled_one_gets` mounts
/// both through the real function and holds their `vike_exec::engine_snapshot::state_hash` equal, a
/// check that catches a divergence a shared-code arrangement could only have prevented by
/// construction and could not have reported.
///
/// Every step below is the same call the post-match tail makes for a paper venue, in the same order
/// (the merge/arm/rescue ordering is load-bearing — see the tail's own comments), with the values a
/// paper arm leaves it: no leg grids, no venue-fetched limits, no reconcile handle, no contract
/// multiplier, `MarginMode::Cross`.
///
/// ⚠ **Takes `account` for the SAME reason the live twin does, and until 2026-09-27 this function
/// did not — a real defect, not a placement choice.** A venue that is paper because
/// `policy.venues.<venue>` refused it got a MORE PERMISSIVE risk engine than the same venue paper
/// for lacking credentials, because only the BOX-WIDE `max_account_exposure` fold below was applied
/// here; `policy.account_exposure.<venue>.<LABEL>` — the PER-ACCOUNT figure, reachable even on a
/// single-account box, see `vike_config::VenuePolicy::account_exposure`'s own doc — was silently
/// skipped. The guard test `a_capped_venue_is_the_same_engine_an_uncredentialled_one_gets` could not
/// catch it because neither `MountPolicy` it constructs sets an account-exposure row.
pub(crate) fn paper_engine(
    venue: &str,
    symbol: &str,
    account: &AccountLabel,
    declared_legs: &[String],
    risk_profile: Option<&vike_exec::ProfileRisk>,
    policy: Option<&crate::MountPolicy>,
) -> EngineAndRecon {
    let static_default = vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, symbol));
    let mut limits = vike_exec::RiskLimits::new();
    // A capped venue resolves no per-leg grid, exactly as every paper arm does — so a declared leg
    // is judged on the mounted symbol's tick/lot/floors, and says so.
    // `NoGrid`: these limits carry no venue grid, so the function returns before it reads the source.
    symbol_grid::warn_ungridded_legs(
        venue,
        vike_bridge_core::venue_mount::DeclaredGridSource::NoGrid,
        symbol,
        declared_legs,
        &limits.grid_by_symbol,
        &limits,
    );
    limits = merge_operator_budget(venue, limits, risk_profile);
    limits = arm_universal_defaults(limits);
    limits.im_requirement = limits.im_requirement.or(Some(1.0));
    // The ACCOUNT-aggregate ceiling, folded here for the same reason every other budget line above
    // is: **a paper mount must not be the PERMISSIVE side of the mount it rehearses.** A rehearsal
    // whose gate admits orders the live arm would refuse teaches an operator that a configuration
    // is safe when it is not — the same standing rule `vike_sim::SimBroker::gate_order` states
    // for the backtest, which was measured to be the permissive side once already. It costs
    // nothing: `None` is off, and a paper engine's book is folded only when the ceiling is armed.
    //
    // ⚠ `policy` rather than `risk_profile` for the box-wide figure and the per-account one right
    // below it, which is why this function takes a parameter it otherwise would not —
    // `vike_config::Policy::max_account_exposure` argues why the number lives in the policy FILE.
    // ⚠ `narrow_account_exposure`, not an assignment, for the reason spelled out at the live twin
    // in `crate::make_engine_for_account`: the fold is a `min`, so this ceiling cannot be RAISED by
    // any later writer of the field.
    limits.narrow_account_exposure(policy.and_then(|p| p.max_account_exposure));
    // …AND THE SAME CEILING, STATED FOR THIS ONE ACCOUNT — the fold this function was missing until
    // 2026-09-27 (see the function's own doc). Two folds rather than a choice between them, exactly
    // as the live twin does it: `narrow_account_exposure` composes to `min(box, account)` with
    // either side absent falling through.
    limits.narrow_account_exposure(policy.and_then(|p| p.venues.account_exposure(venue, account)));
    // The SIZING-EQUITY ceiling, folded here for the same reason the account ceiling above it is:
    // a paper mount must not be the PERMISSIVE side of the mount it rehearses. A rehearsal whose
    // strategies size against a bigger equity than the live arm would gives an operator position
    // sizes they will not get. `None` is off and costs nothing.
    limits.narrow_sizing_equity(policy.and_then(|p| p.max_sizing_equity));
    // ⚠ `require_live_risk_budget` is NOT called, and must not be: this venue mounts PAPER, and the
    // refusal is deliberately scoped to a mount the operator intends LIVE. Refusing to start over
    // an unbounded budget on a venue the deployment's own ceiling just disarmed would be the same
    // false refusal oanda's `resolve` and fxcm's (`FxcmVenueMount` in
    // `crates/bridges/fxcm/src/mount.rs`) exist to avoid.
    let fee_schedule = resolve_fee_schedule(venue, None, static_default);
    tracing::info!(venue, ?fee_schedule, "effective fee schedule");
    // The SAME type-erased shape every venue arm produces — spelled out because this is the one
    // assembly with a single arm, so inference would otherwise pin `Box<PaperExecutionClient>` and
    // hand back a different `ExecutionEngine<_>` from the one `EngineAndRecon` names.
    let client: Box<dyn vike_exec::ExecutionClient + Send> =
        Box::new(paper_client(venue, symbol, static_default));
    let mut engine = vike_exec::ExecutionEngine::new(
        vike_exec::Account::new(
            1.0,
            venue,
            multiplier_grid(symbol, 0.0),
            vike_exec::BalanceMode::Delta,
        )
        .with_default_margin_modes(margin_mode_grid(symbol, vike_model::MarginMode::Cross)),
        vike_exec::RiskGate::new(limits),
        client,
        venue,
        symbol,
    );
    engine.fee_schedule = Some(fee_schedule);
    (engine, None)
}

/// Say, once per capped venue, that the deployment's own file is what put it on paper — naming the
/// venue, the FILE and the KEY, so the line answers "why is this venue paper?" without a grep.
///
/// ⚠ **The LEVEL is the whole design of this function.** A `warn!` for every venue a `paper`
/// ceiling covers would fire ~14 times on every start of a box that has never configured a venue,
/// which is the ordinary unconfigured state and the shape
/// `crates/vike-config/src/arming.rs` names as the way a refusal list teaches operators to work
/// around it. So the level is decided by whether the ceiling actually REFUSED anything: a venue
/// whose credentials would have armed it gets `warn!` (an outcome CHANGED), and a venue with no
/// credentials at all gets `debug!` (nothing was refused — absent credentials were already the
/// gate).
///
/// That question is [`would_mount_under_some_tier`] — DEMO or LIVE — which is PURE: it reads the
/// caller's own `vars` map through each arm's config loader, opens no file, dials nothing and signs
/// nothing. It is the one thing the capped path evaluates that touches credential MATERIAL, and it
/// is worth the exception precisely because the alternative is a line that cannot tell an operator
/// whether their file changed anything.
///
/// ⚠ **Not `crate::would_mount_live`** — that probes ONLY the `Live` ceiling, and decision 0095 means a
/// venue holding only DEMO keys for binance/bybit/okx/hyperliquid now probes `false` there (a `live`
/// ceiling no longer falls back to demo creds for those four), which would misreport a genuinely
/// demo-credentialed venue as having "no live credentials either" — the exact wrong half of this
/// function's split.
pub(crate) fn report_capped_to_paper(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) {
    let declared = policy.is_some_and(|p| p.venues.is_declared());
    if would_mount_under_some_tier(registry, venue, vars) {
        tracing::warn!(
            venue,
            declared,
            "ARMING CEILING: {venue} has credentials, and this deployment's ceiling for it is \
             `paper` → staying PAPER. No credential was read, no instrument grid fetched and no \
             socket opened for it. Run `vike-cli config set policy.venues.{venue} <mode>` (one of \
             paper / demo / live) to change that."
        );
    } else {
        tracing::debug!(
            venue,
            declared,
            "arming ceiling for {venue} is `paper` (the policy.venues.{venue} row); it has no \
             credentials either, so nothing was refused"
        );
    }
}

/// **The upgrade warning, fired at most ONCE per process** — a box that has credentials and has
/// never written a `[venues]` table drops EVERY venue to paper on its first start after the ceiling
/// begins to bite, and must not discover that from a fills report.
///
/// # Why WARN-AND-PAPER rather than refuse
///
/// `docs/decisions/0013-degrade-vs-refuse.md`'s first question is whether the thing that failed is a
/// PROTECTION or a CAPABILITY. A venue connection is a capability (that record lists it by name), so
/// the disposition is degrade — and its third question is satisfied too, because paper strictly
/// REDUCES authority: the worst case is a missed trade, and nothing acts on live venue state.
///
/// The concrete half matters more than the taxonomy: this fires on an UPGRADE, on a running
/// deployment, and a refusal there strands resting orders on nine venues behind a daemon that will
/// not come up. A node that will not start cannot flatten a position either — which is the exact
/// case that record names as the thing that would reopen its verdict, so it is answered rather than
/// invoked. `crates/vike-config/src/arming.rs` argues the same shape for the credential-file
/// escalation and lands on a refusal instead, correctly: an appended real-money switch row (a
/// still-live one like `flags.poly_exec`, or a retired `{VENUE}_MAINNET`-shaped one decision 0095 now
/// refuses on sight) is either an operator asking for MORE authority or a stale line that arms
/// nothing and should not sit there looking like configuration — either way, refusing costs an
/// honest operator nothing they had.
///
/// # Self-silencing, and what silences it
///
/// It fires only while BOTH hold: at least one venue has credentials that WOULD have armed it, and
/// no policy file has ever named a venue under `[venues]`
/// (`vike_config::VenuePolicy::is_declared`). A table with every venue at `paper` silences it —
/// that operator has stated their arming and chosen paper, and a warning that keeps firing on a
/// correct configuration is the "refusal list that fires on harmless lines" `arming.rs` warns
/// about. So does a table naming one venue: the operator has met the key, and the per-venue
/// `report_capped_to_paper` line still covers every venue they left out.
///
/// A deployment with no credentials at all never sees it: nothing was refused, and there is nothing
/// to paste.
///
/// # ⚠ Venue slugs only
///
/// The message names venues and the two settings PATHS. It never prints a credential key NAME (let
/// alone a value) — the same rule `refuse_credential_file_arming` follows, and for a stronger
/// reason here: this text is designed to be pasted, and a paste-ready block that carries key names
/// invites the reply that carries values.
///
/// Returns the message rather than logging it, so the decision is testable with no subscriber; the
/// `Once` latch lives at the call site.
///
/// Public so the roster tests in `crates/vike-tradehub/tests/mount_roster.rs` can reach it
/// (docs/decisions/0096: roster tests run where the registry is).
pub fn venue_arming_migration_message(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> Option<String> {
    if policy.is_some_and(|p| p.venues.is_declared()) {
        return None;
    }
    // Decision 0095: NOT `would_mount_live` — that probes ONLY the `Live` ceiling, and for
    // binance/bybit/okx/hyperliquid a `live` probe with only DEMO keys is now PAPER (those four no
    // longer fall back to demo creds), so it would silently drop a demo-only-credentialed venue
    // from this report. `would_mount_under_some_tier` asks the true question: does this venue have
    // credentials for EITHER tier.
    let credentialled: Vec<&'static str> = vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| would_mount_under_some_tier(registry, v, vars))
        .collect();
    if credentialled.is_empty() {
        return None;
    }
    let mut out = String::from(
        "ARMING CEILING NOW IN FORCE: this box's settings carry no policy.venues rows, so EVERY \
         venue on this box is capped to `paper` — including the ones whose credentials would have \
         armed them before this build.\n\n\
         Credential presence used to be the only gate, which is why a run profile naming one venue \
         could hold live authenticated sessions on nine. The ceiling is the setting that says which \
         venues this deployment MEANS to trade, and its default is the safe end.\n\n\
         These venues have credentials and are now PAPER:\n\n",
    );
    for venue in &credentialled {
        out.push_str(&format!("  {venue}\n"));
    }
    out.push_str(
        "\nTo restore them, run one of these — and CHOOSE each value (`live` is real money; `demo` \
         is the venue's own demo/testnet account; `paper` keeps it simulated). For most roster \
         venues a ceiling can only ever REFUSE: it declines to stop the credentials and the mount \
         arm from reaching the tier they resolve to.",
    );
    // ⚠ Venue slugs only (see this fn's doc): the switched-venue explainer below names ONLY the
    // four-venue subset that is BOTH switched (decision 0095) AND actually credentialled on THIS
    // box — never the full binance/bybit/okx/hyperliquid set unconditionally. A box with, say,
    // only bybit credentials must never see "binance" in this message: it has nothing to do with
    // this box, and `the_migration_warning_names_the_command_and_only_venue_slugs` pins exactly
    // that (a venue absent from `credentialled` may not appear anywhere in the text).
    let switched: Vec<&'static str> =
        credentialled.iter().copied().filter(|v| network_is_the_ceiling(v)).collect();
    if !switched.is_empty() {
        let list = switched.join("/");
        out.push_str(&format!(
            " For {list} the ceiling now SELECTS the tier outright (decision 0095) — the lines \
             below name `demo` for them because that is what this box actually traded before (a \
             `{{VENUE}}_MAINNET` flag could never have been set on a box that reaches this \
             message, since a SET one refuses this process's own boot); `live` for them would be a \
             DELIBERATE new choice, never a restoration, and this message never pastes it for you."
        ));
    }
    out.push_str("\n\n");
    for venue in &credentialled {
        // Decision 0095: for binance/bybit/okx/hyperliquid the ceiling ALONE chooses the network
        // now, and a box that reaches this message could never have had `{VENUE}_MAINNET` set (a
        // set one refuses this process's own boot, before any code here runs). Under the OLD,
        // pre-ceiling behaviour those four venues therefore NEVER resolved mainnet without that
        // flag — so whatever LIVE-tier keys additionally sit in the store today, this box's actual
        // PRIOR trading was DEMO, unconditionally, and the "restore" line must say so: `live` is a
        // NEW choice this message must never make on an operator's behalf, only report as possible
        // in the framing sentence above. Every other venue was never gated by that flag, so it
        // keeps asking the real probe.
        let mode = if network_is_the_ceiling(venue) {
            "demo"
        } else if would_mount_live_under(registry, venue, vars, vike_config::VenueMode::Live) {
            "live"
        } else {
            "demo"
        };
        out.push_str(&format!("vike-cli config set policy.venues.{venue} {mode}\n"));
    }
    out.push_str(
        "\nWriting even one of these silences this message, whatever the values — including \
         `paper`, which is a decision like any other. `vike-cli config show --filter policy` \
         prints what the binaries will read.\n",
    );
    Some(out)
}

/// **The venues whose NETWORK the ceiling chooses** (decision 0095) — the store migration's own
/// list, `vike_secrets::live_means_mainnet::SWITCHED_VENUES` — as
/// [`venue_arming_migration_message`] needs them: a box that reaches that message traded these on
/// DEMO before the ceiling existed, so its restore line never pastes `live` for them.
///
/// ⚠ It was `crate::arming`'s `ceiling_selects_mainnet` (the three CEX venues) plus a separate
/// hyperliquid equality at each of the message's two sites, until the last legacy arm went
/// (docs/decisions/0096) and took that helper's other reader with it. It is a membership test on
/// that constant rather than a venue list of this crate's own: the same four venues, and exactly
/// the set whose `{VENUE}_MAINNET` switch this message's restore argument is about.
fn network_is_the_ceiling(venue: &str) -> bool {
    vike_secrets::live_means_mainnet::SWITCHED_VENUES.contains(&venue)
}

/// The `Once` latch over [`venue_arming_migration_message`]: [`make_engine_with_legs`] runs once
/// per venue, and this text names every venue at once.
///
/// Same idiom, and the same reason, as `vike_bridge_core::halt::halt_path_from_env`'s
/// once-per-process armability report: the fact is process-wide, the site that notices it is
/// per-venue, and repeating a paste-ready block fourteen times would bury it.
pub(crate) fn venue_arming_migration(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Some(message) = venue_arming_migration_message(registry, vars, policy) {
            tracing::warn!("{message}");
        }
    });
}
