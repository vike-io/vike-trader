use super::*;
use std::assert_matches;
use std::path::PathBuf;

use vike_bridge_core::venue_adapter::ArmingHeldBelowLive;
use vike_bridge_core::venue_adapter_fixture::{
    AdapterFixture, ExecutionClientInert, found_tier_events,
};
use vike_log::capture::captured;

/// The account's grant — the two fields the loader requires of the ACCOUNT.
const GRANT: &[(&str, &str)] = &[("ACCESS_TOKEN", "at"), ("REFRESH_TOKEN", "rt")];

/// The venue's application pair — the two fields the loader requires of the VENUE.
const APP: &[(&str, &str)] = &[("CLIENT_ID", "cid"), ("CLIENT_SECRET", "csecret")];

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A fixture whose account holds `grant` and whose venue holds `app`, for the account `label`, on the
/// release seed's addresses (a box's `venue_url` table before an operator moves a row).
fn holding(label: &AccountLabel, grant: &[(&str, &str)], app: &[(&str, &str)]) -> AdapterFixture {
    let mut fx = AdapterFixture::new(grant).with_venue_fields(app);
    fx.account = label.clone();
    fx.settings = vike_secrets::venue_setting::VenueSettings::default()
        .with_urls(vike_venues::venue_urls::VenueUrls::seed(VENUE));
    fx
}

/// The seed's demo addresses, as `start` looks them up.
fn seed_demo() -> CtraderUrls {
    ctrader_host(&vike_venues::venue_urls::VenueUrls::seed(VENUE), AccountTier::Demo)
        .expect("the seed holds the demo rows")
}

/// The complete grant — the account's two tokens and the venue's app pair — for `label`.
fn granted(label: &AccountLabel) -> AdapterFixture {
    holding(label, GRANT, APP)
}

#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = VenueAdapterCtrader.declaration();
    assert_eq!(VenueAdapterCtrader.venue(), "ctrader");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_reconcile_trigger);
    assert_eq!(d.grid_source, GridSource::InHand);
    assert_eq!(
        d.book_identity,
        BookIdentityRule::Named { name_fields: &["ACCOUNT_ID"], evm_key_fields: &[] }
    );
    assert_matches!(d.clock, ClockDeclaration::NotWired { unmeasured_risk: None, .. });
}

/// The arming-probe row at a DEMO account (and a `Paper` tier, read as demo): a complete grant arms
/// a demo-only arm; half a grant — either owner's half — is none.
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed = ArmingResolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(ArmingHeldBelowLive::DemoOnlyArm),
    };
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        assert_eq!(
            VenueAdapterCtrader.resolve(&AdapterFixture::new(&[]).inputs(tier)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
        );
        assert_eq!(
            VenueAdapterCtrader.resolve(&granted(&AccountLabel::Default).inputs(tier)),
            armed
        );
        for half in [
            holding(&AccountLabel::Default, GRANT, &[]),
            holding(&AccountLabel::Default, &[], APP),
            holding(&AccountLabel::Default, &GRANT[..1], APP),
            holding(&AccountLabel::Default, GRANT, &APP[..1]),
        ] {
            assert_eq!(
                VenueAdapterCtrader.resolve(&half.inputs(tier)),
                ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
                "half a grant is no grant ({tier})"
            );
        }
    }
}

/// Review Focus 2: this arm dials only the demo host, so no account tier and no grant makes this
/// venue resolve `Live`.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let fx = granted(&AccountLabel::Default);
    for tier in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        assert!(!matches!(
            VenueAdapterCtrader.resolve(&fx.inputs(tier)),
            ArmingResolution::Armed { tier: AccountTier::Live, .. }
        ));
    }
}

/// **A LIVE account holding its grant is a NAMED cause.** This arm never dials a live account's
/// grant, so the account does not run — and says WHICH fact keeps it there, rather than the words of
/// an empty store. Only a COMPLETE grant counts: half of one is what the loader calls absent.
#[test]
fn a_live_account_holding_its_grant_stays_paper_for_a_named_reason() {
    assert_eq!(
        VenueAdapterCtrader.resolve(&granted(&AccountLabel::Default).inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
    );
    assert_eq!(
        VenueAdapterCtrader.resolve(&granted(&alt()).inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired),
        "a labelled live account the same"
    );
    assert_eq!(
        VenueAdapterCtrader
            .resolve(&holding(&AccountLabel::Default, &GRANT[..1], APP).inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
        "half a live grant is no grant"
    );
}

/// The stay-paper half at the START, through the REAL `live_runtime_for_account` (the unreplaced
/// seam): a live account returns before any socket is dialled, so this is offline, and the outcome
/// is paper with nothing reconciled.
#[test]
fn a_live_account_starts_paper_offline() {
    let fx = granted(&AccountLabel::Default);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Live, "EURUSD", &tx);
    req.reconcile_enabled = true;
    let out = VenueAdapterCtrader.start(req);
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// …and the live-mount step is never even ASKED for a live account, with or without its grant: a
/// live account's grant must not reach a seam that dials. (A double that panics when asked.)
#[test]
fn a_live_account_never_reaches_the_live_start_step() {
    for fx in [granted(&AccountLabel::Default), AdapterFixture::new(&[])] {
        let (tx, _rx) = vike_execution::event_channel(8);
        let out = start_with(
            fx.request(AccountTier::Live, "EURUSD", &tx),
            |_, _, _, _, _, _, _, _, _| panic!("a live account: nothing may be dialled"),
        );
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    }
}

/// THE PROCESS FACT THIS BRIDGE STOPPED READING FOR ITSELF. The rotation home — the settings
/// directory whose store a refreshed grant is written back to, the account row it is written back
/// to, and the state directory its change journal is kept in — is derived from the state directory
/// [`grant`] is HANDED and the credentials' own account; handed no directory, there is no rotation
/// home at all, which is what every test and tool always got.
///
/// The directory reaches [`grant`] in two hops. `start` hands `AdapterInputs::process.state_dir` to
/// [`live_runtime_for_account`], which `start_forwards_the_requests_own_values_to_the_live_start`
/// pins; that function hands it to `grant` on its first line, which no offline test drives — with a
/// grant present it goes on to dial the demo trade socket.
#[test]
fn grant_takes_the_rotation_home_from_the_state_directory_it_is_handed() {
    let mut fx = granted(&AccountLabel::Default);
    fx.account_id = Some(7);
    let state = PathBuf::from("project/settings/state");
    let none = CtraderUrls::default();
    let persist = grant(&fx.credentials(), &none, Some(state.as_path()))
        .expect("the grant is present")
        .token_persist
        .expect("a handed state directory arms rotation");
    assert_eq!(persist.state_dir, state, "the change journal's home is the handed directory");
    assert_eq!(
        persist.settings_dir,
        PathBuf::from("project/settings"),
        "the write-back target is the settings directory the handed one sits in"
    );
    assert_eq!(persist.account_row, Some(7), "…and the account the grant was read from");
    assert!(
        grant(&fx.credentials(), &none, None).expect("present").token_persist.is_none(),
        "handed none, a refreshed grant lives as long as the process"
    );
    assert!(
        grant(&AccountCredentials::none(), &none, Some(state.as_path())).is_none(),
        "a state directory is not a credential"
    );
}

/// Absent credentials: paper, nothing reconciled, nothing dialled — whatever state directory the
/// start is handed.
#[test]
fn with_no_credentials_the_start_is_paper_and_offline() {
    let mut fx = AdapterFixture::new(&[]);
    fx.process.state_dir = Some(PathBuf::from("project/settings/state"));
    let (tx, _rx) = vike_execution::event_channel(8);
    let out = VenueAdapterCtrader.start(fx.request(AccountTier::Demo, "EURUSD", &tx));
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// **No demo row, no dial.** A complete grant on a table that cannot answer the demo addresses is
/// NOT RUNNING: the start says so once at `error!`, naming the row, and never asks the live-mount
/// step; the credential probe FAILS with the same words instead of dialling.
#[test]
fn a_demo_row_the_table_cannot_answer_is_refused_by_name_and_dials_nothing() {
    let mut fx = granted(&AccountLabel::Default);
    fx.settings = vike_secrets::venue_setting::VenueSettings::default();
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        start_with(fx.request(AccountTier::Demo, "EURUSD", &tx), |_, _, _, _, _, _, _, _, _| {
            panic!("no address: nothing may be dialled")
        })
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    let said: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
    assert_eq!(said.len(), 1, "{events:?}");
    assert!(format!("{:?}", said[0]).contains("trade socket"), "the row is named: {events:?}");
    let Some(CredentialProbe::ReadOnly(probe)) =
        VenueAdapterCtrader.credential_probe(&fx.inputs(AccountTier::Demo))
    else {
        panic!("a grant is probed");
    };
    assert!(probe().expect_err("no address").contains("trade socket"));
}

/// What [`start_with`]'s live-mount step was handed, as it saw it.
struct Forwarded {
    account: AccountLabel,
    credentials_account_row: Option<i64>,
    credentials_access_token: Option<String>,
    endpoints: CtraderUrls,
    state_dir: Option<PathBuf>,
    symbol: String,
    reconcile_enabled: bool,
    events_are_the_requests: bool,
    halt_admit: HaltAdmit,
    halt_path: PathBuf,
}

/// `start` hands its live-mount step the REQUEST'S OWN values — the account being started, its
/// credentials, the state directory it was handed, the symbol, the global reconcile gate, the
/// account-scoped event lane, the resolved halt-admit mode and the HALT sentinel's path — none
/// defaulted and none re-derived.
/// Every value is one a default cannot imitate (a labelled account, an account row other than the
/// fixture's default, a handed directory, a symbol that is not the probe's, reconcile on,
/// `Verify`), so a dropped argument reads as its default and fails here. Two carry the weight:
/// `addresses_accounts: true` promises one engine per ACCOUNT, and cTrader is the one venue where
/// `HaltAdmit::Verify` acts at all. The step answers `None`, which is the paper outcome.
#[test]
fn start_forwards_the_requests_own_values_to_the_live_start() {
    let mut fx = granted(&alt());
    fx.account_id = Some(42);
    fx.process.state_dir = Some(PathBuf::from("project/settings/state"));
    fx.process.halt_path = PathBuf::from("project/settings/state/HALT");
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "GBPUSD", &tx);
    req.reconcile_enabled = true;
    req.halt_admit = HaltAdmit::Verify;
    let mut seen = None;
    let out = start_with(
        req,
        |account,
         credentials,
         endpoints,
         state_dir,
         symbol,
         recon_enabled,
         events,
         halt,
         halt_path| {
            seen = Some(Forwarded {
                account: account.clone(),
                credentials_account_row: credentials.account_id(),
                credentials_access_token: credentials.get("ACCESS_TOKEN").map(str::to_string),
                endpoints: endpoints.clone(),
                state_dir: state_dir.map(Path::to_path_buf),
                symbol: symbol.to_string(),
                reconcile_enabled: recon_enabled,
                events_are_the_requests: std::ptr::eq(events, &tx),
                halt_admit: halt,
                halt_path: halt_path.to_path_buf(),
            });
            None
        },
    );
    let seen = seen.expect("mount asked its live-mount step");
    assert_eq!(seen.account, alt(), "the account being mounted, never the default one");
    assert_eq!(seen.credentials_account_row, Some(42), "the request's own account's credentials");
    assert_eq!(seen.credentials_access_token.as_deref(), Some("at"), "…its fields, as handed");
    assert_eq!(seen.endpoints, seed_demo(), "the account's own DEMO rows, never the live ones");
    assert_eq!(
        seen.state_dir,
        Some(PathBuf::from("project/settings/state")),
        "the rotation home the mount was handed"
    );
    assert_eq!(seen.symbol, "GBPUSD", "the mounted symbol");
    assert!(seen.reconcile_enabled, "the global reconcile gate");
    assert!(seen.events_are_the_requests, "the request's account-scoped event lane");
    assert_eq!(seen.halt_admit, HaltAdmit::Verify, "the resolved halt-admit mode");
    assert_eq!(
        seen.halt_path,
        PathBuf::from("project/settings/state/HALT"),
        "the HALT sentinel the mount was handed — the client watches it, nothing it resolves itself"
    );
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
        "no live mount is the paper outcome"
    );
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// A reconcile client that answers one known balance, so the outcome's handle can be told apart
/// from any other, and reports nothing else.
struct KnownBalance;

impl ReconcileClient for KnownBalance {
    fn fetch_order_status_reports(
        &self,
        _since: i64,
    ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(
        &self,
    ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        Ok(Some(4242.0))
    }
}

/// A handshake's symbol map holding the engine symbol and one declared leg on DIFFERENT grids
/// (`digits` and the volume grid both differ), so answering one symbol for the other shows.
fn handshake_symbols() -> Arc<SymbolMap> {
    use crate::transport::proto::{ProtoOaLightSymbol, ProtoOaSymbol};
    let light = vec![
        ProtoOaLightSymbol {
            symbol_id: 1,
            symbol_name: Some("EURUSD".to_string()),
            ..Default::default()
        },
        ProtoOaLightSymbol {
            symbol_id: 2,
            symbol_name: Some("GBPUSD".to_string()),
            ..Default::default()
        },
    ];
    let full = vec![
        ProtoOaSymbol {
            symbol_id: 1,
            digits: 5,
            lot_size: Some(10_000_000),
            min_volume: Some(100_000),
            step_volume: Some(100_000),
            max_volume: Some(10_000_000_000),
            ..Default::default()
        },
        ProtoOaSymbol {
            symbol_id: 2,
            digits: 4,
            lot_size: Some(10_000_000),
            min_volume: Some(1_000_000),
            step_volume: Some(1_000_000),
            max_volume: Some(1_000_000_000),
            ..Default::default()
        },
    ];
    Arc::new(SymbolMap::from_symbols(&light, &full))
}

/// THE LIVE HALF, offline: with the handshake answered by a double, `start` folds what it produced
/// into the contract's Live outcome — bound to the DEMO tier (the only tier this venue starts), the
/// engine symbol's grid and each KNOWN declared leg's off the handshake's own symbol map (an
/// unknown leg gets no row, for `vike-tradehub-core`'s fold to report), no contract size, no margin mode,
/// the handshake's reconcile client passed straight through, and no identity record.
#[test]
fn a_live_handshake_is_the_demo_tiers_live_outcome() {
    let fx = granted(&AccountLabel::Default);
    let (tx, _rx) = vike_execution::event_channel(8);
    let legs = vec!["GBPUSD".to_string(), "NOT-LISTED".to_string()];
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    req.declared_legs = legs.as_slice();
    let symbols = handshake_symbols();
    let runtime = symbols.risk_properties("EURUSD").expect("EURUSD is in the handshake");
    let leg = symbols.risk_properties("GBPUSD").expect("GBPUSD is in the handshake");
    assert_ne!(runtime, leg, "the fixture's two symbols sit on different grids");
    let out = start_with(req, |_, _, _, _, _, _, _, _, _| {
        Some(CtraderLiveRuntime {
            client: Box::new(ExecutionClientInert),
            reconcile: Some(Box::new(KnownBalance)),
            symbols,
        })
    });
    let VenueAdapterExecutionOutcome::Running(live) = out.exec else {
        panic!("a completed handshake is a LIVE outcome");
    };
    assert_eq!(live.bound_tier, AccountTier::Demo, "the tier the DEMO grant authenticates");
    assert_eq!(live.grid, Some(runtime), "the MOUNTED symbol's grid");
    assert!(live.contract_value.is_none() && live.margin_mode.is_none());
    assert_eq!(
        live.leg_grids,
        vec![("GBPUSD".to_string(), leg)],
        "each known declared leg's own grid; an unknown leg gets no row"
    );
    assert_eq!(
        out.reconcile.expect("the handshake's reconcile client").fetch_balance(),
        Ok(Some(4242.0)),
        "passed straight through"
    );
    assert!(out.identity.is_none());
}

/// The LAZY read-only probe (was `vike-tradehub-core`'s `authed_read_probes` ctrader branch). Building it
/// touches nothing — `recon_client` OPENS AND AUTHENTICATES a protobuf socket, so it runs inside the
/// closure, where a refused grant becomes the leg's FAIL instead of a missing row — and this test is
/// offline BECAUSE of that laziness: the closure is never called. An eager build would dial here
/// and — these credentials being fake — answer `None`, failing the first assertion. It reads the
/// account it is asked about, and never a live account's grant.
///
/// The handshake resolves a symbol in the account's own list, so the probe takes the one the account's row
/// STATES (owner, 2026-11-13: never a compiled one): a row that states none gets a probe that FAILS naming the row,
/// with nothing dialled (calling it here is offline for that reason).
#[test]
fn a_credentialed_store_gets_a_lazy_read_only_probe() {
    let mut stated = granted(&AccountLabel::Default);
    stated.accounts = vike_secrets::account_directory::AccountDirectory::from_rows(
        vike_secrets::AccountRows::Known(vec![vike_secrets::AccountRow {
            symbol: Some("EURUSD".to_string()),
            ..vike_secrets::account_row(1, VENUE, AccountTier::Demo, None, true)
        }]),
        None,
    );
    assert!(matches!(
        VenueAdapterCtrader.credential_probe(&stated.inputs(AccountTier::Demo)),
        Some(CredentialProbe::ReadOnly(_))
    ));
    let Some(CredentialProbe::ReadOnly(unstated)) = VenueAdapterCtrader
        .credential_probe(&granted(&AccountLabel::Default).inputs(AccountTier::Demo))
    else {
        panic!("a grant is probed");
    };
    let why = unstated().expect_err("no symbol stated: the probe fails, dialling nothing");
    assert!(why.contains("`account.symbol`"), "{why}");
    assert!(
        VenueAdapterCtrader
            .credential_probe(&AdapterFixture::new(&[]).inputs(AccountTier::Demo))
            .is_none()
    );
    assert!(
        VenueAdapterCtrader
            .credential_probe(&granted(&AccountLabel::Default).inputs(AccountTier::Live))
            .is_none(),
        "a live account's grant is never dialled, so it is never probed"
    );
}

// ── what the arm says about a LIVE account ──────────────────────────────────────────────────────

/// A completed handshake answered by an offline double — what an armed start needs, dialling nothing.
fn offline_live_runtime(
    _: &AccountLabel,
    _: &AccountCredentials<'_>,
    _: &CtraderUrls,
    _: Option<&Path>,
    _: &str,
    _: bool,
    _: &EventSender,
    _: HaltAdmit,
    _: &Path,
) -> Option<CtraderLiveRuntime> {
    Some(CtraderLiveRuntime {
        client: Box::new(ExecutionClientInert),
        reconcile: None,
        symbols: Arc::new(SymbolMap::from_symbols(&[], &[])),
    })
}

/// A live account holding its complete grant says so, ONCE, at `error!` — `found_tier`, never
/// `tier`, and no credential value — and does not run.
#[test]
fn a_live_account_holding_its_grant_is_named_once_and_starts_paper() {
    let named = AccountLabel::parse("NAMED").expect("a legal label");
    let fx = holding(&named, &[("ACCESS_TOKEN", SECRET), ("REFRESH_TOKEN", SECRET)], APP);
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) =
        captured(|| start_with(fx.request(AccountTier::Live, "EURUSD", &tx), offline_live_runtime));
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
        "a live account is never mounted"
    );
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "exactly one line about the live account: {events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("ctrader"), Some("NAMED")), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a diagnostic carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// A demo account holding its grant — the ordinary case — says nothing about a live tier.
#[test]
fn a_demo_account_says_nothing_about_the_live_tier() {
    let alone = AccountLabel::parse("ALONE").expect("a legal label");
    let fx = granted(&alone);
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) =
        captured(|| start_with(fx.request(AccountTier::Demo, "EURUSD", &tx), offline_live_runtime));
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::Running(_)),
        "the demo grant mounts, as it did"
    );
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
}

/// An INCOMPLETE grant on a live account — half the tokens, the venue's app pair alone, or the
/// tokens without the app pair — is what the loader calls absent: the ordinary, SILENT
/// `NoCredentials`, at the start and for an account that is never started alike, and still paper.
#[test]
fn an_incomplete_grant_on_a_live_account_is_silent_no_credentials() {
    let half = AccountLabel::parse("HALF").expect("a legal label");
    for fx in [
        holding(&half, &GRANT[..1], APP),
        holding(&half, &[], APP),
        holding(&half, &[("ACCESS_TOKEN", SECRET), ("REFRESH_TOKEN", SECRET)], &[]),
    ] {
        let (tx, _rx) = vike_execution::event_channel(8);
        let (out, events) = captured(|| {
            start_with(fx.request(AccountTier::Live, "EURUSD", &tx), |_, _, _, _, _, _, _, _, _| {
                panic!("a live account: nothing may be dialled")
            })
        });
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
        assert!(found_tier_events(&events).is_empty(), "{events:?}");
        let (_, events) = captured(|| {
            VenueAdapterCtrader.report_not_running_account(&fx.inputs(AccountTier::Live))
        });
        assert!(found_tier_events(&events).is_empty(), "{events:?}");
        assert_eq!(
            VenueAdapterCtrader.resolve(&fx.inputs(AccountTier::Live)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
            "the cause the arming screen prints"
        );
    }
}

/// A LABELLED account is started only when it armed, so a live one holding its grant never reaches
/// `start` and said nothing at all — `vike-tradehub-core` asks the arm to speak for it instead, and the arm
/// says exactly what `start` says.
#[test]
fn an_account_that_is_never_started_is_spoken_for_in_the_same_words() {
    let unmounted = AccountLabel::parse("UNMNT").expect("a legal label");
    let live = granted(&unmounted);
    let (_, events) = captured(|| {
        VenueAdapterCtrader.report_not_running_account(&live.inputs(AccountTier::Live))
    });
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    let (_, events) = captured(|| {
        VenueAdapterCtrader
            .report_not_running_account(&AdapterFixture::new(&[]).inputs(AccountTier::Live));
    });
    assert!(events.is_empty(), "an empty store is the ordinary state and is silent: {events:?}");

    let (_, events) = captured(|| {
        VenueAdapterCtrader.report_not_running_account(&live.inputs(AccountTier::Demo))
    });
    assert!(events.is_empty(), "a demo account has no live tier to name: {events:?}");
}
