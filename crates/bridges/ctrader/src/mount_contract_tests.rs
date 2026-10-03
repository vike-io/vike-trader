use super::*;
use std::path::PathBuf;

use vike_bridge_core::venue_mount_fixture::{MountFixture, captured, found_tier_events};
use vike_model::account_keys::account_key;

use crate::token_store::TokenKeys;

/// The four keys the DEMO-tier grant needs: the app registration pair and this tier's tokens.
const KEYS: &[(&str, &str)] = &[
    ("CTRADER_CLIENT_ID", "cid"),
    ("CTRADER_CLIENT_SECRET", "csecret"),
    ("CTRADER_DEMO_ACCESS_TOKEN", "at"),
    ("CTRADER_DEMO_REFRESH_TOKEN", "rt"),
];

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A fixture holding `KEYS` under `label`'s key names, asking for `label`.
fn keyed(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in KEYS {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx.account = label.clone();
    fx
}

#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = CtraderVenueMount.declaration();
    assert_eq!(CtraderVenueMount.venue(), "ctrader");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::InHand);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named {
            prefix: "CTRADER",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE"],
            name_suffixes: &["ACCOUNT_ID"],
            evm_key_suffixes: &[],
        }
    );
    assert!(matches!(d.clock, ClockDecl::NotWired { unmeasured_risk: None, .. }));
}

/// The arming-probe row as it was: a complete DEMO grant arms a demo-only arm; half a grant is
/// none.
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed =
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };
    for live in [false, true] {
        assert_eq!(
            CtraderVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
        assert_eq!(CtraderVenueMount.resolve(&keyed(&AccountLabel::Default).inputs(live)), armed);
        assert_eq!(
            CtraderVenueMount.resolve(&MountFixture::new(&KEYS[..3]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "half a grant is no grant"
        );
    }
}

/// Review Focus 2: the OAuth shape loaded is always the DEMO tier's, so no ceiling and no
/// live-tier grant makes this venue resolve `Live`.
///
/// The LIVE tier's token names are COMPOSED by this crate's own naming function
/// ([`TokenKeys::for_env`]) and never spelled: a bare `CTRADER_LIVE_*` literal in a `src/` file
/// reads to `crates/vike-ops/tests/settings_registry.rs` as an `Injected` map read, against this
/// crate's rows for those two names, which are declared `TestOnly`
/// (`declared_layer_matches_the_path`).
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let mut fx = keyed(&AccountLabel::Default);
    let live_keys = TokenKeys::for_env(Environment::Live);
    fx.vars.insert(live_keys.access, "lat".to_string());
    fx.vars.insert(live_keys.refresh, "lrt".to_string());
    for live in [false, true] {
        assert!(!matches!(
            CtraderVenueMount.resolve(&fx.inputs(live)),
            Resolution::Armed { tier: Tier::Live, .. }
        ));
    }
}

/// A complete LIVE-tier grant for `label` and NO demo grant: the app registration pair plus the
/// LIVE tier's two tokens, the token names COMPOSED by this crate's own naming function for the
/// reason [`a_ceiling_below_live_never_resolves_live`] gives. `with_refresh: false` leaves the
/// refresh token out, which is half a grant.
fn live_grant_only(label: &AccountLabel, with_refresh: bool) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in &KEYS[..2] {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    let live_keys = TokenKeys::for_account(Environment::Live, label);
    fx.vars.insert(live_keys.access, "lat".to_string());
    if with_refresh {
        fx.vars.insert(live_keys.refresh, "lrt".to_string());
    }
    fx.account = label.clone();
    fx
}

/// **A LIVE-tier grant the arm cannot use is a NAMED cause.** This venue mounts the DEMO grant
/// whatever the ceiling says, so a store holding only the LIVE grant arms nothing — and says WHICH
/// tier it will not use, rather than that the store holds nothing. Only a COMPLETE grant counts:
/// half of one is what the loader calls absent.
#[test]
fn a_live_grant_alone_stays_paper_for_a_named_reason() {
    for live in [false, true] {
        assert_eq!(
            CtraderVenueMount.resolve(&live_grant_only(&AccountLabel::Default, true).inputs(live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "live={live}"
        );
        assert_eq!(
            CtraderVenueMount.resolve(&live_grant_only(&AccountLabel::Default, false).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "half a LIVE grant is no grant (live={live})"
        );
    }
    // Beside a complete DEMO grant the venue arms exactly as it always did.
    let mut both = keyed(&AccountLabel::Default);
    let live_keys = TokenKeys::for_env(Environment::Live);
    both.vars.insert(live_keys.access, "lat".to_string());
    both.vars.insert(live_keys.refresh, "lrt".to_string());
    assert_eq!(
        CtraderVenueMount.resolve(&both.inputs(true)),
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) }
    );
}

/// The cause is scoped to the ACCOUNT asking, in both directions.
#[test]
fn the_live_tier_cause_is_scoped_to_the_account_that_holds_the_grant() {
    let mut default_live = live_grant_only(&AccountLabel::Default, true);
    default_live.account = alt();
    assert_eq!(
        CtraderVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT asks, and only the DEFAULT account holds a live grant"
    );
    let mut alt_live = live_grant_only(&alt(), true);
    assert_eq!(
        CtraderVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    alt_live.account = AccountLabel::Default;
    assert_eq!(
        CtraderVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the DEFAULT account asks, and only ALT holds a live grant"
    );
}

/// The stay-paper half at the MOUNT, through the REAL `live_mount_for_account` (the unreplaced
/// seam): a LIVE grant alone returns before any socket is dialled — the demo grant it would have
/// dialled with is absent — so this is offline, and the outcome is paper with nothing reconciled.
#[test]
fn a_live_grant_alone_mounts_paper_offline() {
    let fx = live_grant_only(&AccountLabel::Default, true);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, "EURUSD", &tx);
    req.recon_enabled = true;
    let out = CtraderVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// …and the live-mount step is never even ASKED: a store the arm cannot use must not reach a
/// seam that dials. (A double that panics when asked.)
#[test]
fn a_live_grant_alone_never_reaches_the_live_mount_step() {
    let fx = live_grant_only(&AccountLabel::Default, true);
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = mount_with(fx.request(true, "EURUSD", &tx), |_, _, _, _, _, _, _, _| {
        panic!("the demo grant is absent: nothing may be dialled")
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
}

/// The account 2×2 — the app pair is labelled too, with no fallback (the loader's own doc argues
/// why), so a labelled account never reads the default account's grant.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        CtraderVenueMount.resolve(&alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default account's grant"
    );
    assert!(matches!(
        CtraderVenueMount.resolve(&keyed(&alt()).inputs(true)),
        Resolution::Armed { .. }
    ));
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        CtraderVenueMount.resolve(&default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// THE PROCESS FACT THIS BRIDGE STOPPED READING FOR ITSELF. The rotation home — the store a
/// refreshed grant is written back to, and the state directory its change journal is kept in — is
/// derived from the state directory [`grant`] is HANDED; handed none, there is no rotation home at
/// all, which is what every test and tool always got.
///
/// The directory reaches [`grant`] in two hops. `mount` hands `MountInputs::process.state_dir` to
/// [`live_mount_for_account`], which `mount_forwards_the_requests_own_values_to_the_live_mount`
/// pins; that function hands it to `grant` on its first line, which no offline test drives — with a
/// grant present it goes on to dial `demo.ctraderapi.com`.
#[test]
fn grant_takes_the_rotation_home_from_the_state_directory_it_is_handed() {
    let fx = keyed(&AccountLabel::Default);
    let state = PathBuf::from("project/settings/state");
    let persist = grant(&AccountLabel::Default, &fx.vars, Some(state.as_path()))
        .expect("the grant is present")
        .token_persist
        .expect("a handed state directory arms rotation");
    assert_eq!(persist.state_dir, state, "the change journal's home is the handed directory");
    assert_eq!(
        persist.store,
        PathBuf::from("project/settings").join(vike_secrets::SECRETS_FILE),
        "the write-back target is the store beside the handed directory"
    );
    assert!(
        grant(&AccountLabel::Default, &fx.vars, None).expect("present").token_persist.is_none(),
        "handed none, a refreshed grant lives as long as the process"
    );
    assert!(
        grant(&AccountLabel::Default, &HashMap::new(), Some(state.as_path())).is_none(),
        "a state directory is not a credential"
    );
}

/// Absent credentials: paper, nothing reconciled, nothing dialled — whatever state directory the
/// mount is handed.
#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let mut fx = MountFixture::new(&[]);
    fx.process.state_dir = Some(PathBuf::from("project/settings/state"));
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = CtraderVenueMount.mount(fx.request(true, "EURUSD", &tx));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// What [`mount_with`]'s live-mount step was handed, as it saw it.
struct Forwarded {
    account: AccountLabel,
    secrets_are_the_inputs: bool,
    state_dir: Option<PathBuf>,
    symbol: String,
    recon_enabled: bool,
    events_are_the_requests: bool,
    halt_admit: HaltAdmit,
    halt_path: PathBuf,
}

/// `mount` hands its live-mount step the REQUEST'S OWN values — the account being mounted, its
/// secrets, the state directory it was handed, the symbol, the global reconcile gate, the
/// account-scoped event lane, the resolved halt-admit mode and the HALT sentinel's path — none
/// defaulted and none re-derived.
/// Every value is one a default cannot imitate (a labelled account, a handed directory, a symbol
/// that is not the probe's, reconcile on, `Verify`), so a dropped argument reads as its default and
/// fails here. Two carry the weight: `addresses_accounts: true` promises one engine per ACCOUNT, and
/// cTrader is the one venue where `HaltAdmit::Verify` acts at all. The step answers `None`, which is
/// the paper outcome.
#[test]
fn mount_forwards_the_requests_own_values_to_the_live_mount() {
    let mut fx = keyed(&alt());
    fx.process.state_dir = Some(PathBuf::from("project/settings/state"));
    fx.process.halt_path = PathBuf::from("project/settings/state/HALT");
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, "GBPUSD", &tx);
    req.recon_enabled = true;
    req.halt_admit = HaltAdmit::Verify;
    let mut seen = None;
    let out = mount_with(
        req,
        |account, secrets, state_dir, symbol, recon_enabled, events, halt, halt_path| {
            seen = Some(Forwarded {
                account: account.clone(),
                secrets_are_the_inputs: std::ptr::eq(secrets, &fx.vars),
                state_dir: state_dir.map(Path::to_path_buf),
                symbol: symbol.to_string(),
                recon_enabled,
                events_are_the_requests: std::ptr::eq(events, &tx),
                halt_admit: halt,
                halt_path: halt_path.to_path_buf(),
            });
            None
        },
    );
    let seen = seen.expect("mount asked its live-mount step");
    assert_eq!(seen.account, alt(), "the account being mounted, never the default one");
    assert!(seen.secrets_are_the_inputs, "the request's own secrets");
    assert_eq!(
        seen.state_dir,
        Some(PathBuf::from("project/settings/state")),
        "the rotation home the mount was handed"
    );
    assert_eq!(seen.symbol, "GBPUSD", "the mounted symbol");
    assert!(seen.recon_enabled, "the global reconcile gate");
    assert!(seen.events_are_the_requests, "the request's account-scoped event lane");
    assert_eq!(seen.halt_admit, HaltAdmit::Verify, "the resolved halt-admit mode");
    assert_eq!(
        seen.halt_path,
        PathBuf::from("project/settings/state/HALT"),
        "the HALT sentinel the mount was handed — the client watches it, nothing it resolves itself"
    );
    assert!(matches!(out.exec, ExecOutcome::Paper), "no live mount is the paper outcome");
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// The live exec client's slot, filled offline: a client that sends nothing anywhere.
struct Inert;

impl ExecutionClient for Inert {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// A reconcile client that answers one known balance, so the outcome's handle can be told apart
/// from any other, and reports nothing else.
struct KnownBalance;

impl ReconClient for KnownBalance {
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

/// A handshake's symbol map holding the mounted symbol and one declared leg on DIFFERENT grids
/// (`digits` and the volume grid both differ), so answering one symbol for the other shows.
fn handshake_symbols() -> Arc<SymbolMap> {
    use crate::proto::{ProtoOaLightSymbol, ProtoOaSymbol};
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

/// THE LIVE HALF, offline: with the handshake answered by a double, `mount` folds what it produced
/// into the contract's Live outcome — bound to the DEMO tier (the only tier this venue mounts,
/// whatever the ceiling), the mounted symbol's grid and each KNOWN declared leg's off the
/// handshake's own symbol map (an unknown leg gets no row, for `vike-mount`'s fold to report), no
/// contract size, no margin mode, the handshake's reconcile client passed straight through, and no
/// identity record.
#[test]
fn a_live_handshake_is_the_demo_tiers_live_outcome() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_exec::event_channel(8);
    let legs = vec!["GBPUSD".to_string(), "NOT-LISTED".to_string()];
    let mut req = fx.request(true, "EURUSD", &tx);
    req.declared_legs = legs.as_slice();
    let symbols = handshake_symbols();
    let mounted = symbols.risk_properties("EURUSD").expect("EURUSD is in the handshake");
    let leg = symbols.risk_properties("GBPUSD").expect("GBPUSD is in the handshake");
    assert_ne!(mounted, leg, "the fixture's two symbols sit on different grids");
    let out = mount_with(req, |_, _, _, _, _, _, _, _| {
        Some(CtraderMount { client: Box::new(Inert), recon: Some(Box::new(KnownBalance)), symbols })
    });
    let ExecOutcome::Live(live) = out.exec else {
        panic!("a completed handshake is a LIVE outcome");
    };
    assert_eq!(live.bound_tier, Tier::Demo, "the tier the DEMO grant authenticates");
    assert_eq!(live.grid, Some(mounted), "the MOUNTED symbol's grid");
    assert!(live.contract_size.is_none() && live.margin_mode.is_none());
    assert_eq!(
        live.leg_grids,
        vec![("GBPUSD".to_string(), leg)],
        "each known declared leg's own grid; an unknown leg gets no row"
    );
    assert_eq!(
        out.recon.expect("the handshake's reconcile client").fetch_balance(),
        Ok(Some(4242.0)),
        "passed straight through"
    );
    assert!(out.identity.is_none());
}

/// The LAZY read-only probe (was `vike-mount`'s `authed_read_probes` ctrader branch). Building it
/// touches nothing — `recon_client` OPENS AND AUTHENTICATES a protobuf socket, so it runs inside the
/// closure, where a refused grant becomes the leg's FAIL instead of a missing row — and this test is
/// offline BECAUSE of that laziness: the closure is never called. An eager build would dial here
/// and — these credentials being fake — answer `None`, failing the first assertion. It reads the
/// DEFAULT account's grant, exactly as the branch did.
#[test]
fn a_credentialed_store_gets_a_lazy_read_only_probe() {
    assert!(matches!(
        CtraderVenueMount.credential_probe(&keyed(&AccountLabel::Default).inputs(true)),
        Some(CredentialProbe::ReadOnly(_))
    ));
    assert!(CtraderVenueMount.credential_probe(&MountFixture::new(&[]).inputs(true)).is_none());
    assert!(
        CtraderVenueMount.credential_probe(&keyed(&alt()).inputs(true)).is_none(),
        "the probe reads the DEFAULT account's grant, as the branch it replaced did"
    );
}

// ── what the arm says about a LIVE tier it will not use ─────────────────────────────────────────
//
// Every account label below is unique to its test: the unused-beside-demo `warn!` is said once per
// process per `(venue, account)`, so two tests sharing a label would pass or fail by scheduling order.

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The DEMO grant AND a complete LIVE grant's two tokens (the app pair is shared), both for `label`.
fn both_tiers(label: &AccountLabel) -> MountFixture {
    let mut fx = keyed(label);
    let live_keys = TokenKeys::for_account(Environment::Live, label);
    fx.vars.insert(live_keys.access, SECRET.to_string());
    fx.vars.insert(live_keys.refresh, SECRET.to_string());
    fx
}

/// A completed handshake answered by an offline double — what an armed mount needs, dialling nothing.
#[allow(clippy::too_many_arguments)] // the seam's own eight
fn offline_live_mount(
    _: &AccountLabel,
    _: &HashMap<String, String>,
    _: Option<&Path>,
    _: &str,
    _: bool,
    _: &EventSender,
    _: HaltAdmit,
    _: &Path,
) -> Option<CtraderMount> {
    Some(CtraderMount {
        client: Box::new(Inert),
        recon: None,
        symbols: Arc::new(SymbolMap::from_symbols(&[], &[])),
    })
}

/// A live grant BESIDE the demo one used to mount the demo tier in silence. It still mounts the demo
/// tier — bound at DEMO, nothing else moves — and now says, once, that the live grant is unused.
#[test]
fn a_live_grant_beside_the_demo_one_is_named_unused_and_the_mount_is_unchanged() {
    let beside = AccountLabel::parse("BESIDE").expect("a legal label");
    let fx = both_tiers(&beside);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) =
        captured(|| mount_with(fx.request(true, "EURUSD", &tx), offline_live_mount));
    let ExecOutcome::Live(exec) = out.exec else { panic!("the demo grant mounts, as it did") };
    assert_eq!(exec.bound_tier, Tier::Demo, "the live grant must not move the bound tier");
    let unused = found_tier_events(&events);
    assert_eq!(unused.len(), 1, "exactly one line about the live grant: {events:?}");
    let e = unused[0];
    assert_eq!(e.level, tracing::Level::WARN, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("ctrader"), Some("BESIDE")), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a diagnostic carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// The demo grant alone — the ordinary case — says nothing new.
#[test]
fn a_demo_grant_alone_adds_no_line_about_the_live_tier() {
    let alone = AccountLabel::parse("ALONE").expect("a legal label");
    let fx = keyed(&alone);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (_, events) = captured(|| mount_with(fx.request(true, "EURUSD", &tx), offline_live_mount));
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
}

/// A live grant missing one token is a typo or a half-finished edit, and the loader calls it
/// absent — so it printed `NoCredentials` under the words of an empty store. The mount now names
/// the key it lacks (label-composed, names only) and still lands on paper.
#[test]
fn a_half_written_live_grant_names_the_missing_key_and_the_mount_is_still_paper() {
    let half = AccountLabel::parse("HALF").expect("a legal label");
    let fx = live_grant_only(&half, false);
    let live_keys = TokenKeys::for_account(Environment::Live, &half);
    let (missing, present) = (live_keys.refresh, live_keys.access);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) =
        captured(|| mount_with(fx.request(true, "EURUSD", &tx), |_, _, _, _, _, _, _, _| None));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("ctrader"), Some("HALF")), "{e:?}");
    assert!(e.message.contains(&missing), "names the missing key `{missing}`: {}", e.message);
    assert!(!e.message.contains(&present), "names only what is MISSING: {}", e.message);
    assert_eq!(
        CtraderVenueMount.resolve(&fx.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the cause the arming screen prints is unchanged"
    );
}

/// The app registration pair is shared by every tier and names none: a store holding ONLY the pair
/// has started no live grant, so nothing is said about the live tier's tokens being missing.
#[test]
fn the_shared_app_pair_alone_starts_no_live_grant() {
    let pair_only = AccountLabel::parse("PAIR").expect("a legal label");
    let mut fx = MountFixture::new(&[]);
    fx.account = pair_only.clone();
    for (k, v) in &KEYS[..2] {
        fx.vars.insert(account_key(k, &pair_only), (*v).to_string());
    }
    let (tx, _rx) = vike_exec::event_channel(8);
    let (_, events) =
        captured(|| mount_with(fx.request(true, "EURUSD", &tx), |_, _, _, _, _, _, _, _| None));
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
}

/// A live grant whose APP PAIR is missing is half-written too, and the pair is what is named.
#[test]
fn live_tokens_without_the_app_pair_name_the_pair() {
    let tokens_only = AccountLabel::parse("TOKENS").expect("a legal label");
    let mut fx = MountFixture::new(&[]);
    fx.account = tokens_only.clone();
    let live_keys = TokenKeys::for_account(Environment::Live, &tokens_only);
    fx.vars.insert(live_keys.access, SECRET.to_string());
    fx.vars.insert(live_keys.refresh, SECRET.to_string());
    let (_, events) = captured(|| CtraderVenueMount.report_unmounted_account(&fx.inputs(true)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    for (name, _) in &KEYS[..2] {
        let name = account_key(name, &tokens_only);
        assert!(said[0].message.contains(&name), "names `{name}`: {}", said[0].message);
    }
    assert!(!format!("{:?}", said[0]).contains(SECRET), "no credential VALUE: {:?}", said[0]);
}

/// A LABELLED account is mounted only when it armed, so one holding only live keys never reaches
/// `mount` and said nothing at all — `vike-mount` asks the arm to speak for it instead, and the arm
/// says exactly what `mount` says for the default account.
#[test]
fn an_account_that_is_never_mounted_is_spoken_for_in_the_same_words() {
    let unmounted = AccountLabel::parse("UNMNT").expect("a legal label");
    let live_only = live_grant_only(&unmounted, true);
    let (_, events) =
        captured(|| CtraderVenueMount.report_unmounted_account(&live_only.inputs(true)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    let (_, events) = captured(|| {
        CtraderVenueMount.report_unmounted_account(&MountFixture::new(&[]).inputs(true));
    });
    assert!(events.is_empty(), "an empty store is the ordinary state and is silent: {events:?}");
}
