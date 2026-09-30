//! The cTrader LIVE-MOUNT seam (decision 0088, step B5) — the one factory
//! `vike_mount::make_engine`'s `("ctrader", _)` arm calls, so the venue FACTS that arm used to spell
//! inline (which env-var shape this venue's OAuth grant takes, how to open the authed exec socket,
//! how to open the SEPARATE authed reconcile socket, and that `HaltAdmit::Verify` is real here
//! because this is the one adapter holding a live position book at its halt boundary) live in this
//! crate instead, next to the rest of the venue's wiring — the same cut `crates/bridges/polymarket`'s
//! `live_mount_for_account` and `crates/bridges/oanda`'s `mountable_tier_for_account` already made.
//!
//! ## What moved, and what deliberately did not
//!
//! [`live_mount_for_account`] now does everything the mount arm used to do inline between "the
//! ceiling let us reach this venue" and "here is a client and maybe a recon handle": load the OAuth
//! config (with its credential-rotation home), open the dedicated reconcile socket FIRST when
//! reconciliation is wanted, run the blocking protobuf/TLS exec handshake, and wrap the result with
//! the operator's halt-admit mode. What it does NOT do is anything about the ARMING CEILING itself —
//! `vike_mount::make_engine_for_account` already refuses to reach this venue's match arm at all once
//! `policy.venues.ctrader` resolves to `Paper`, and cTrader has no second tier switch of its own
//! (there is no `CTRADER_MAINNET`-shaped flag; the OAuth shape this factory loads is always the
//! `Demo`-tier one, whether the ceiling that let the arm run was `Demo` or `Live` — a live-token
//! flip is a deliberate code change, not a runtime switch). So unlike a venue whose bridge has to
//! ask "am I allowed to go live" ([`vike_model::account_keys::AccountLabel`] aside, decision 0088's
//! B1/B3 steps thread a `live_permitted: bool` for exactly that question), this factory needs no
//! ceiling input at all — passing one would be a parameter nothing here would ever read.
//!
//! The per-symbol RISK GRID stays the composition root's job on purpose: `vike_mount::symbol_grid`
//! is a shared home every live arm consults uniformly, not a cTrader fact, and this crate must not
//! learn a second copy of that fold. So [`CtraderMount`] hands back the resolved
//! [`crate::symbols::SymbolMap`] the exec handshake already paid for (an `Arc` clone, no further
//! network cost) rather than a pre-built grid, and the caller asks it the same two questions
//! (`risk_properties(symbol)` for the mounted symbol, `risk_properties(leg)` per declared leg) it
//! always has.
//!
//! The generic halt-admit REPORT (`report_halt_admit`/`report_halt_admit_armed` — written once, for
//! every venue, because only the mount knows whether an armed request actually reached a live
//! client or fell back to paper) also stays in `vike_mount`; this factory only takes the resolved
//! [`vike_model::HaltAdmit`] as a plain value and applies it.

use std::collections::HashMap;
use std::sync::Arc;

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::halt::declared_project_state_dir;
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::HaltAdmit;
use vike_model::account_keys::AccountLabel;

use crate::config::CtraderConfig;
use crate::conn::connect_and_auth_exec;
use crate::exec::CtraderExec;
use crate::recon_client::recon_client;
use crate::symbols::SymbolMap;

/// A live cTrader mount: the exec client, its dedicated reconcile client (when one was requested
/// and connected), and the exec handshake's own resolved instrument grid.
pub struct CtraderMount {
    /// Boxed for `vike_mount::make_engine`'s `Box<dyn ExecutionClient + Send>` slot.
    pub client: Box<dyn ExecutionClient + Send>,
    /// `Some` when reconcile was requested AND the dedicated authed socket connected — opened on
    /// its OWN handshake (mirrors deribit/aster), so a reconcile read never contends the exec
    /// actor's protobuf socket.
    pub recon: Option<Box<dyn ReconClient>>,
    /// The exec handshake's own resolved symbol map, shared (an `Arc`, no further network call) so
    /// the caller can fill both the mounted symbol's live `RiskLimits` and every declared leg's
    /// `SymbolGrid` at no extra cost — see the module doc for why that fold stays in `vike_mount`.
    pub symbols: Arc<SymbolMap>,
}

/// Build the live cTrader mount for one account, straight from the workspace credential map.
///
/// Returns `None` — leaving the caller on its paper fallback — for either of the two cases that
/// were byte-identical before this factory existed:
/// * **no OAuth grant configured** for this account (`CTRADER_CLIENT_ID`/`_SECRET` +
///   `CTRADER_DEMO_ACCESS_TOKEN`/`_REFRESH_TOKEN`, this account's own names, via
///   [`CtraderConfig::from_vars_with_store_for_account`]): absent credentials ARE the live gate, and
///   no network call is made;
/// * **the blocking protobuf/TLS handshake failed**: cTrader connects SYNCHRONOUSLY here, unlike
///   the crypto/deribit/aster arms' self-healing actors, so a transient outage demotes the whole
///   session to paper for this run rather than retrying in the background (a fast-follow noted on
///   `vike_mount::make_engine`'s ctrader arm). Any reconcile socket already opened for this attempt
///   is dropped along with it — a paper venue holds no reconcile handle, exactly like the
///   absent-creds path.
///
/// `state_dir`, the credential-ROTATION home cTrader's rotating refresh token needs, is read here
/// from [`vike_bridge_core::halt::declared_project_state_dir`] — the same process-wide boot fact
/// `crate::exec`'s own halt-sentinel default already reads directly from this module. It names a
/// fact about THIS PROCESS's deployment, resolved once at boot by `vike-boot`, not a value any
/// composition root actually varies per call — which is why it is read here rather than threaded in
/// as a parameter.
pub fn live_mount_for_account(
    account: &AccountLabel,
    vars: &HashMap<String, String>,
    symbol: &str,
    recon_enabled: bool,
    events: &EventSender,
    halt_admit: HaltAdmit,
) -> Option<CtraderMount> {
    let cfg = CtraderConfig::from_vars_with_store_for_account(
        Environment::Demo,
        account,
        vars,
        declared_project_state_dir().as_deref(),
    )?;
    // Reconcile FIRST, on its OWN dedicated authed socket (mirrors the deribit/aster arms): a
    // reconcile report fetch must never contend the exec actor's protobuf socket. `None` — either
    // reconcile is off, or the handshake failed — leaves this mount reconcile-inert; exec is
    // unaffected either way. cTrader has no `PropertiesRecorder` and takes no `recon_trigger`
    // (interval-only reconcile, like deribit/aster).
    //
    // LAZY: with reconciliation off, this second protobuf/TLS OAuth handshake is never attempted —
    // `recon_client` is simply not called, matching
    // `vike_bridge_core::venue_mount::recon_if_enabled`'s contract for every other inline-recon venue.
    let recon = if recon_enabled { recon_client(&cfg, symbol) } else { None };
    // A no-op live-data sink: this mount wires exec only — market data comes from the separate
    // `CtraderData` feed the composition root wires elsewhere.
    let sink: Arc<dyn vike_data::LiveDataSink> = Arc::new(vike_data::TeeSink(Vec::new()));
    match connect_and_auth_exec(cfg.to_conn_config(), sink, events.clone()) {
        Ok(handle) => {
            tracing::warn!(
                "ctrader: DEMO credentials present → LIVE exec client (real demo orders)"
            );
            // The exec handshake's own resolved grid, cloned (an `Arc`, no network) BEFORE `handle`
            // is consumed below — so the caller can still ask it about the mounted symbol and every
            // declared leg at no further cost.
            let symbols = handle.symbols.clone();
            // THE halt-admit consumer. cTrader is the ONE venue where `HaltAdmit::Verify` does
            // anything (`vike_model::halt_verify_support`), because it is the only adapter holding
            // a position book at its halt boundary — seeded at connect, kept fresh from every
            // execution event. `Admit` (the default, and every deployment with no `policy.toml`) is
            // byte-identical to the flag-trusting rule cTrader got in #1180.
            let client: Box<dyn ExecutionClient + Send> =
                Box::new(CtraderExec::new(handle, events.clone()).with_halt_admit(halt_admit));
            Some(CtraderMount { client, recon, symbols })
        }
        Err(e) => {
            // Demote to PAPER for this attempt (see the weaker-robustness note on
            // `vike_mount::make_engine`'s ctrader arm). Any recon socket opened moments ago is now
            // incoherent — it would reconcile a PAPER engine against LIVE cTrader state — so it is
            // dropped here with the rest of this function's locals, never returned.
            tracing::warn!(
                error = %e,
                "ctrader: exec connect/auth failed → falling back to PAPER for this session"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absent credentials ⇒ `None` and **no network call** — the CI-safe half, mirroring
    /// `vike_polymarket::exec_plane::mount`'s `without_a_usable_key_the_mount_is_none_and_offline`: this test
    /// must return before `connect_and_auth_exec` ever dials a socket.
    #[test]
    fn without_credentials_the_mount_is_none_and_offline() {
        let (tx, _rx) = vike_exec::event_channel(8);
        let vars = HashMap::new();
        assert!(
            live_mount_for_account(
                &AccountLabel::Default,
                &vars,
                "EURUSD",
                true,
                &tx,
                HaltAdmit::default()
            )
            .is_none()
        );
    }

    /// A half-written grant (app pair present, tier tokens absent) is the same live gate as no
    /// grant at all — still no network call.
    #[test]
    fn a_half_written_grant_is_also_none_and_offline() {
        let (tx, _rx) = vike_exec::event_channel(8);
        let mut vars = HashMap::new();
        vars.insert("CTRADER_CLIENT_ID".to_string(), "app-id".to_string());
        vars.insert("CTRADER_CLIENT_SECRET".to_string(), "app-secret".to_string());
        assert!(
            live_mount_for_account(
                &AccountLabel::Default,
                &vars,
                "EURUSD",
                true,
                &tx,
                HaltAdmit::default()
            )
            .is_none()
        );
    }
}
