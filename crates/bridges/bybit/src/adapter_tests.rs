use super::*;
use vike_bridge_core::venue_adapter::ArmingNotRunningCause;
use vike_bridge_core::venue_adapter_fixture::{
    AdapterFixture, ExecutionClientInert, assert_one_key_set_resolves_its_tier,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::credential_keys::{API_KEY, API_SECRET, attribution_var_for};
use vike_venues::venue_urls::VenueUrls;

/// A fixture whose started account holds the complete pair (`API_KEY` = `"k"`, `API_SECRET` =
/// `"s"`) — the account's fields, by the field words `api_credentials` reads (decision 0156 item 6:
/// no name is composed, so none is spelled here) — and the release seed's addresses.
fn paired() -> AdapterFixture {
    let mut fx = AdapterFixture::new(&[(API_KEY, "k"), (API_SECRET, "s")]);
    fx.settings = VenueSettings::default().with_urls(VenueUrls::seed(VENUE));
    fx
}

/// A fixture whose started account holds the key and NOT the secret — a half-written pair.
fn half_paired() -> AdapterFixture {
    AdapterFixture::new(&[(API_KEY, "k")])
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The REAL body bybit answered, captured from the CI box on 2026-08-09 (moved from `vike-tradehub-core`, whose
/// `fixture()` helper read it).
fn clock_body() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/bybit.json"))
        .expect("the captured fixture is valid JSON")
}

/// What `start_with` hands its spawn step for `req` — `(broker_id, fast_execution, leverage, whether a
/// reconnect trigger rides along)` — with every network step replaced by an offline double.
fn spawned_with(req: AdapterRequest<'_>) -> (Option<String>, bool, f64, bool) {
    let mut seen = None;
    let _ = start_with(
        req,
        |_, _, _, _| None,
        |_, _, _, _, _| None,
        |spawn| {
            seen = Some((
                spawn.broker_id,
                spawn.fast_execution,
                spawn.leverage,
                spawn.reconcile_trigger.is_some(),
            ));
            Box::new(ExecutionClientInert)
        },
    );
    seen.expect("the account's key pair spawns the exec client")
}

/// The rows `vike-tradehub-core`'s tables carried for bybit, pinned as a matrix.
#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = VenueAdapterBybit.declaration();
    assert_eq!(VenueAdapterBybit.venue(), "bybit");
    assert!(d.addresses_accounts && d.process_exclusive.is_none());
    assert!(d.takes_reconcile_trigger, "the resync supervisor pokes the reconcile driver");
    assert_eq!(d.grid_source, GridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentityRule::Undeterminable {
            why: "the store holds an HMAC api key/secret pair and no account identifier; a bybit \
                  sub-account is selected BY THE KEY, which names it nowhere offline",
        }
    );
    assert_eq!(
        d.clock,
        ClockDeclaration::Wired {
            endpoint: "GET /v5/market/time (public)",
            auth: ClockAuthentication::Public,
            risk: ClockRisk::SignedTimestamp,
        }
    );
}

/// THE D1 MATRIX (docs/decisions/0095, decision 0156 item 6): the account's tier is the network,
/// and its key pair is the one it holds. A complete pair arms the account's own tier; no pair (or
/// half of one) is paper, and under a `live` tier the cause names the missing LIVE key set.
#[test]
fn resolve_reads_exactly_the_tier_the_ceiling_names() {
    let absent = [AdapterFixture::new(&[]), half_paired()];
    assert_one_key_set_resolves_its_tier(&VenueAdapterBybit, absent, &paired());
}

/// Review Focus 2 at this venue: a complete pair under a tier below `live` never arms live —
/// `Paper` reads as `Demo`.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    for below in [AccountTier::Demo, AccountTier::Paper] {
        assert_eq!(
            VenueAdapterBybit.resolve(&paired().inputs(below)),
            ArmingResolution::Armed { tier: AccountTier::Demo, held_below_live: None },
            "tier={below}"
        );
    }
}

/// The account 2×2 — bybit is the venue `vike-tradehub-core`'s `bybit_store_with_alt` fixture was written
/// for: a sub-account is selected BY THE KEY, so ALT's keys are ALT's book. The LABEL picks no key
/// here: the start reads the credentials it is handed — the started account's own, which
/// `vike-tradehub-core` resolved from the account row — so a labelled account arms on its pair and stays
/// paper without one, exactly as the default account does.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_empty = AdapterFixture::new(&[]);
    alt_empty.account = alt();
    assert_eq!(
        VenueAdapterBybit.resolve(&alt_empty.inputs(AccountTier::Demo)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
        "ALT without keys of its own is paper"
    );
    let mut alt_paired = paired();
    alt_paired.account = alt();
    assert_eq!(
        VenueAdapterBybit.resolve(&alt_paired.inputs(AccountTier::Demo)),
        ArmingResolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        "ALT arms on its own keys"
    );
}

/// No complete key pair: paper, nothing to reconcile, nothing recorded — and OFFLINE, which the
/// body shows when its three outward steps are doubles that panic: it returns before the grid
/// pre-fetch, the reconcile factory and the exec spawn.
///
/// ⚠ The doubled body runs FIRST, and the order is the point: were the credential gate ever to
/// regress, the row fails there, on an `unreachable!`, before the real `start` below could run its
/// `instruments-info` pre-fetch and spawn an exec thread against a live host from a unit test.
#[test]
fn without_a_key_set_for_the_ceilings_tier_the_start_is_paper_and_offline() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let empty = AdapterFixture::new(&[]);
    let half = half_paired();
    for (fx, ceiling_live) in [
        (&empty, AccountTier::Demo),
        (&empty, AccountTier::Live),
        (&half, AccountTier::Demo),
        (&half, AccountTier::Live),
    ] {
        let body = start_with(
            fx.request(ceiling_live, "BTCUSDT", &tx),
            |_, _, _, _| unreachable!("no grid pre-fetch without a key set"),
            |_, _, _, _, _| unreachable!("no reconcile client without a key set"),
            |_| unreachable!("no exec spawn without a key set"),
        );
        assert!(
            matches!(body.exec, VenueAdapterExecutionOutcome::NotRunning),
            "ceiling_live={ceiling_live}"
        );
        assert!(body.reconcile.is_none() && body.identity.is_none());
        let out = VenueAdapterBybit.start(fx.request(ceiling_live, "BTCUSDT", &tx));
        assert!(
            matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
            "ceiling_live={ceiling_live}"
        );
        assert!(out.reconcile.is_none() && out.identity.is_none());
    }
    assert!(VenueAdapterBybit.credential_probe(&empty.inputs(AccountTier::Live)).is_none());
}

/// The operator's balance and open-orders reads are the ADAPTER's (base: the trait's default answered `not offered by
/// bybit` to every kind; then the open orders kept those words): with no key pair it is told so and nothing is asked,
/// at either tier, a pair on a box with no address row is told the row, while the positions keep the shared
/// `not offered` words even with a pair (answered before any client is built). Offline: a read with a pair and the
/// rows would reach the venue, so this test asks none.
#[test]
fn the_adapter_answers_the_balance_and_open_orders_reads_and_offers_no_other() {
    let empty = AdapterFixture::new(&[]);
    for tier in [AccountTier::Demo, AccountTier::Live] {
        for kind in [vike_model::VenueReadKind::Balance, vike_model::VenueReadKind::OpenOrders] {
            let why = VenueAdapterBybit.venue_read(&empty.inputs(tier), kind).unwrap_err();
            assert!(why.contains("no bybit api key pair"), "{tier} {kind:?}: {why}");
        }
    }
    let no_rows = AdapterFixture::new(&[(API_KEY, "k"), (API_SECRET, "s")]);
    let why = VenueAdapterBybit
        .venue_read(&no_rows.inputs(AccountTier::Demo), vike_model::VenueReadKind::OpenOrders)
        .unwrap_err();
    assert!(why.contains("no address row"), "the missing row is said: {why}");
    let kind = vike_model::VenueReadKind::Positions;
    let why = VenueAdapterBybit.venue_read(&paired().inputs(AccountTier::Demo), kind).unwrap_err();
    assert_eq!(why, vike_model::not_offered_words(VENUE, kind));
}

/// Was `vike-tradehub-core`'s `credentialed_bybit_yields_a_recon_client` and the bybit line of its
/// `an_armed_mainnet_verdict_still_yields_a_recon_client`. Those asked the reconcile FACTORY alone,
/// which answers `Some` by construction, so no edit to the start could turn them red. This drives
/// the start's own body at both tiers: the reconcile client is built on the network the grid
/// pre-fetch and the exec spawn are handed — the one the account's tier names — and comes back
/// beside the exec client, which is bound at that tier and signs with the account's pair.
#[test]
fn the_start_reconciles_on_the_network_it_trades_on() {
    crate::history::kept_list_for_tests();
    let (tx, _rx) = vike_execution::event_channel(8);
    let fx = paired();
    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        let (mut grid_on, mut reconcile_on, mut spawned_on) = (None, None, None);
        let mut signed_with = None;
        let out = start_with(
            fx.request(ceiling_live, "BTCUSDT", &tx),
            |_, _, rest, _| {
                grid_on = Some(rest.to_string());
                None
            },
            |c, symbol, rest, admitted, clock| {
                reconcile_on = Some(rest.to_string());
                crate::reconcile::reconcile_client_sharing(c, symbol, rest, admitted, clock)
            },
            |spawn| {
                spawned_on = Some(spawn.endpoints.clone());
                signed_with = Some(spawn.creds.api_key.into_inner());
                Box::new(ExecutionClientInert)
            },
        );
        let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
            panic!("the account's key pair mounts live (ceiling_live={ceiling_live})");
        };
        let tier = ceiling_live;
        assert_eq!(exec.bound_tier, tier, "the tier the mount binds");
        let want = crate::execution::rest::seed_endpoints(tier);
        let on = Some(want.rest.clone());
        assert_eq!((grid_on, reconcile_on), (on.clone(), on), "one network for all three");
        assert_eq!(spawned_on, Some(want), "the exec spawn binds the tier's rows");
        assert_eq!(
            signed_with.as_deref(),
            Some("k"),
            "the exec client signs with the account's pair"
        );
        assert!(out.reconcile.is_some(), "the reconcile client comes back beside the exec client");
        assert!(out.identity.is_none());
    }
}

/// The demo host's `instruments-info` row for `symbol` (`tests/fixtures/classes/`), as the grid pre-fetch parses it.
fn captured_row(body: &str, symbol: &str) -> crate::execution::rest::BybitInstrument {
    let payload: serde_json::Value = serde_json::from_str(body).expect("a captured fixture");
    crate::execution::rest::parse_bybit_perp_instruments(&payload)[symbol].clone()
}

/// **A coin-margined MOUNT reports the row's inverse contract value** (decision 0258's bybit amendment), so the
/// engine values `BTCUSD` at one US dollar a contract and keeps its books in BTC; the grid is the row's (a whole
/// contract the step). A linear mount reports nothing, as before.
///
/// Base: the start reported `contract_value: None` for every mount, so the engine valued `BTCUSD` LINEAR.
#[test]
fn a_coin_margined_mount_reports_the_rows_inverse_value() {
    crate::history::kept_list_for_tests();
    let (tx, _rx) = vike_execution::event_channel(8);
    let fx = paired();
    let inverse = include_str!("../tests/fixtures/classes/instruments_inverse_perp_btcusd.json");
    let linear = include_str!("../tests/fixtures/classes/instruments_linear_perp_ethusdt.json");
    for (symbol, body, want) in [
        ("BTCUSD", inverse, Some(vike_model::ContractValue::inverse(1.0))),
        ("ETHUSDT", linear, None),
    ] {
        let out = start_with(
            fx.request(AccountTier::Demo, symbol, &tx),
            |_, _, _, _| Some(captured_row(body, symbol)),
            |_, _, _, _, _| None,
            |_| Box::new(ExecutionClientInert),
        );
        let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
            panic!("{symbol}: the pair mounts live")
        };
        assert_eq!(exec.contract_value, want, "{symbol}");
        let grid = exec.grid.expect("the row's grid");
        assert_eq!(grid.step_size, captured_row(body, symbol).properties.step_size, "{symbol}");
    }
}

/// A mount whose NAME reads as coin-margined and whose row could not be read does NOT RUN (its kind is never
/// defaulted to linear), and nothing is spawned or built; a linear name starts with no grid (its exec thread
/// refuses its orders until the row loads: there is no fallback grid).
#[test]
fn an_unread_coin_margined_mount_does_not_run() {
    crate::history::kept_list_for_tests();
    let (tx, _rx) = vike_execution::event_channel(8);
    let fx = paired();
    let out = start_with(
        fx.request(AccountTier::Demo, "BTCUSD", &tx),
        |_, _, _, _| None,
        |_, _, _, _, _| unreachable!("no reconcile client for a mount that does not run"),
        |_| unreachable!("no exec spawn for a mount that does not run"),
    );
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning) && out.reconcile.is_none()
    );
    let out = start_with(
        fx.request(AccountTier::Demo, "BTCUSDT", &tx),
        |_, _, _, _| None,
        |_, _, _, _, _| None,
        |_| Box::new(ExecutionClientInert),
    );
    let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
        panic!("a linear mount starts without its row")
    };
    assert!(exec.grid.is_none() && exec.contract_value.is_none());
}

/// **One signing clock per start** (owner ruling B-4): the grid pre-fetch, the reconcile client and the exec spawn
/// are handed the SAME `VenueClock`, so the venue time the first of them reads is the one every later signature and
/// the private socket's login stamp.
#[test]
fn a_start_hands_one_signing_clock_to_every_step() {
    crate::history::kept_list_for_tests();
    let (tx, _rx) = vike_execution::event_channel(8);
    let fx = paired();
    let read_at_grid = 7_000;
    let (mut at_reconcile, mut at_spawn) = (None, None);
    let _ = start_with(
        fx.request(AccountTier::Demo, "BTCUSDT", &tx),
        |_, _, _, clock| {
            clock.read_if_due(0, |_| Ok::<_, ()>(read_at_grid)).unwrap();
            None
        },
        |_, _, _, _, clock| {
            at_reconcile = Some(clock.ms_from(|| 0)());
            None
        },
        |spawn| {
            at_spawn = Some(spawn.clock.ms_from(|| 0)());
            Box::new(ExecutionClientInert)
        },
    );
    assert_eq!((at_reconcile, at_spawn), (Some(read_at_grid), Some(read_at_grid)));
}

/// **No address row, no start**: an account whose `venue_url` rows hold no `trade`/`user` host for its type is
/// NOT RUNNING (paper, the refusal logged), and nothing is fetched, built or spawned; its probe, its leverage
/// write and its balance read refuse in the lookup's words. A demo account never borrows the live rows.
#[test]
fn an_account_without_its_address_rows_does_not_run() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut bare = paired();
    bare.settings = VenueSettings::default();
    let mut live_only = paired();
    live_only.settings = VenueSettings::default()
        .with_urls(VenueUrls::seed_typed(VENUE, vike_venues::venue_urls::UrlType::Live));
    for (fx, tier) in
        [(&bare, AccountTier::Demo), (&bare, AccountTier::Live), (&live_only, AccountTier::Demo)]
    {
        let out = start_with(
            fx.request(tier, "BTCUSDT", &tx),
            |_, _, _, _| unreachable!("no grid pre-fetch without a host"),
            |_, _, _, _, _| unreachable!("no reconcile client without a host"),
            |_| unreachable!("no exec spawn without a host"),
        );
        assert!(
            matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning) && out.reconcile.is_none(),
            "{tier}"
        );
        let Some(CredentialProbe::ReadOnly(probe)) =
            credential_probe_with(&fx.inputs(tier), |_, _, _| unreachable!())
        else {
            panic!("a refused address row is a probe that fails, never no probe ({tier})");
        };
        assert!(probe().unwrap_err().contains("no address row"), "{tier}");
        let why = VenueAdapterBybit.set_symbol_leverage(&fx.inputs(tier), "ETHUSDT", 2.0);
        assert!(why.unwrap_err().to_string().contains("no address row"), "{tier}");
        let why =
            VenueAdapterBybit.venue_read(&fx.inputs(tier), vike_model::VenueReadKind::Balance);
        assert!(why.unwrap_err().contains("no address row"), "{tier}");
    }
}

/// The exec client is spawned with what the legacy arm read for it: the FD-broker code out of the
/// deployment's named values, the `execution.fast` hint out of the venue's settings, the leverage
/// out of the operator's `[risk]` budget, and the recon trigger the resync supervisor fires —
/// each ABSENT one reading as the arm's default (no `X-Referer` header, the historical 2x, the fast
/// stream off, no trigger).
#[test]
fn the_execution_client_is_spawned_with_what_the_arm_read() {
    crate::history::kept_list_for_tests();
    let (tx, _rx) = vike_execution::event_channel(8);
    let bare = paired();
    assert_eq!(
        spawned_with(bare.request(AccountTier::Demo, "BTCUSDT", &tx)),
        (None, false, 2.0, false)
    );

    let mut set = paired();
    // The broker-code NAME — a tier-less DEPLOYMENT name, so it rides `AdapterInputs::secrets`, not
    // the account's fields — is asked of the table module rather than spelled out (the literal
    // harvest of `crates/vike-ops/tests/settings_secrets/settings_registry.rs` reads a name spelled
    // in a `src/` file as a variable this crate names); for bybit's `X-Referer` header mechanic it
    // is the `_BROKER_CODE` spelling.
    let broker_code = attribution_var_for(VENUE).expect("bybit stamps an attribution code");
    set.vars.insert(broker_code, "VK42".to_string());
    // `venue.bybit.fast_exec` — a `venue_setting` row, which the start reads from
    // `AdapterInputs::settings` (decision 0095, Task 7).
    set.settings = VenueSettings::from_rows(
        VENUE,
        &[vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: None,
            field: "FAST_EXEC".to_string(),
            value: "1".to_string(),
        }],
    )
    .with_urls(VenueUrls::seed(VENUE));
    let profile = vike_model::ProfileRisk { max_leverage: Some(5.0), ..Default::default() };
    let (trigger, _poked) = mpsc::channel();
    let mut req = set.request(AccountTier::Demo, "BTCUSDT", &tx);
    req.risk_profile = Some(&profile);
    req.reconcile_trigger = Some(trigger);
    assert_eq!(spawned_with(req), (Some("VK42".to_string()), true, 5.0, true));
}

/// THE TIER at the one leg that SIGNS at startup, before any start: the probe signs with the
/// account's pair, on the HOST of the account's tier, and reports that tier — so a `demo` account
/// never sends a signed read to the real-money network. With no complete pair there is no probe at
/// all. Pure — the reconcile client is a signer plus a transport.
#[test]
fn a_demo_ceiling_never_signs_against_the_real_money_tier() {
    let account = paired();
    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        let tier = ceiling_live;
        let mut signed = None;
        let probe = credential_probe_with(&account.inputs(ceiling_live), |c, symbol, rest| {
            signed = Some((c.api_key.expose().clone(), symbol.to_string(), rest.to_string()));
            crate::reconcile::recon_client(c, symbol, rest)
        });
        let Some(CredentialProbe::RecordsIdentity { bound_tier: reported, .. }) = probe else {
            panic!("the account's pair yields an identity-recording probe");
        };
        assert_eq!(reported, tier, "the tier the probe reports (ceiling_live={ceiling_live})");
        assert_eq!(
            signed,
            Some((
                "k".to_string(),
                String::new(),
                crate::execution::rest::seed_endpoints(tier).rest
            )),
            "the key the probe signs with is the account's, the host is its tier's, and no symbol \
             scopes it (its balance read is account-wide)"
        );
    }
    for ceiling_live in [AccountTier::Demo, AccountTier::Live] {
        assert!(VenueAdapterBybit.credential_probe(&half_paired().inputs(ceiling_live)).is_none());
    }
}

// ---- the clock PARSE, against the real captured body (moved from vike-tradehub-core) -------------------

#[test]
fn the_parser_reads_the_real_captured_body() {
    assert_eq!(parse_server_time(&clock_body()), Ok(1_786_242_369_911));
}

/// THE UNIT TRAP: `result.timeNano` is NANOSECONDS as a string and `result.timeSecond` SECONDS —
/// reading either is a millionfold or thousandfold error that still looks like a plausible integer.
#[test]
fn the_parser_does_not_read_a_neighbouring_field_in_the_wrong_unit() {
    let body = clock_body();
    let ns = body["result"]["timeNano"].as_str().expect("the capture carries timeNano");
    let secs = body["result"]["timeSecond"].as_str().expect("…and timeSecond");
    let read = parse_server_time(&body).expect("the ms number parses");
    assert_ne!(read.to_string(), ns, "timeNano is nanoseconds — a millionfold error");
    assert_ne!(read.to_string(), secs, "timeSecond is seconds — a thousandfold error");
    assert_eq!(read / 1_000, secs.parse::<i64>().unwrap(), "…and it agrees with them");
    assert_eq!(read, ns.parse::<i64>().unwrap() / 1_000_000);
}

#[test]
fn a_body_without_the_stamp_names_the_missing_field() {
    assert_eq!(parse_server_time(&serde_json::json!({})), Err(missing_time_field("time")));
}
