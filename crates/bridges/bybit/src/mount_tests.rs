use super::*;
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_model::credential_keys::{
    API_KEY_SUFFIX, API_SECRET_SUFFIX, attribution_var_for, key_owner, starter_keys,
};

/// The DEMO pair `load_credentials_for_account` reads for this venue, as `(name, value)`.
fn demo_pair() -> Vec<(String, &'static str)> {
    vec![("BYBIT_DEMO_API_KEY".to_string(), "dk"), ("BYBIT_DEMO_API_SECRET".to_string(), "ds")]
}

/// The LIVE pair (key, then secret), taken from the credential grid's own enumeration rather than
/// spelled out here: the ports keep LIVE-tier names out of
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s literal harvest, which reads a spelled-out name
/// in a `src/` file as a variable this crate names. ⚠ Asked of the table module, not composed
/// here: the same file's `generated_key_sites` reads any call to one of the grid's builders — and
/// this test module is folded into `mount.rs` there — as "this crate reads the whole grid", and
/// would demand a registry row for every name in it (the reason `vike_model::credential_keys`
/// keeps `starter_keys` and `key_owner` beside the table).
fn live_pair() -> Vec<(String, &'static str)> {
    let tier = Some(Environment::Live.as_str());
    [(API_KEY_SUFFIX, "lk"), (API_SECRET_SUFFIX, "ls")]
        .into_iter()
        .map(|(suffix, value)| {
            let name = starter_keys(VENUE)
                .into_iter()
                .find(|k| k.ends_with(suffix) && key_owner(k) == Some((VENUE, tier)))
                .expect("the credential grid names this venue's LIVE-tier key");
            (name, value)
        })
        .collect()
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// A fixture holding every pair of `sets` under `label`'s key names, asking for `label`.
fn keyed(label: &AccountLabel, sets: &[Vec<(String, &'static str)>]) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (name, value) in sets.iter().flatten() {
        fx.vars.insert(account_key(name, label), (*value).to_string());
    }
    fx.account = label.clone();
    fx
}

/// The REAL body bybit answered, captured from the CI box on 2026-08-09 (moved from `vike-mount`, whose
/// `fixture()` helper read it).
fn clock_body() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/server_time/bybit.json"))
        .expect("the captured fixture is valid JSON")
}

/// An exec client that is never driven: the real one would dial the venue from its own thread the
/// moment it spawned, and these tests read what the mount's body HANDS the spawn.
struct Unspawned;

impl ExecutionClient for Unspawned {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// What `mount_with` hands its spawn step for `req` — `(broker_id, fast_exec, leverage, whether a
/// reconnect trigger rides along)` — with every network step replaced by an offline double.
fn spawned_with(req: MountRequest<'_>) -> (Option<String>, bool, f64, bool) {
    let mut seen = None;
    let _ = mount_with(
        req,
        |_, _, _| None,
        |_, _, _| None,
        |spawn| {
            seen = Some((
                spawn.broker_id,
                spawn.fast_exec,
                spawn.leverage,
                spawn.recon_trigger.is_some(),
            ));
            Box::new(Unspawned)
        },
    );
    seen.expect("a key set at the ceiling's tier spawns the exec client")
}

/// The rows `vike-mount`'s tables carried for bybit, pinned as a matrix.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = BybitVenueMount.declaration();
    assert_eq!(BybitVenueMount.venue(), "bybit");
    assert!(d.addresses_accounts && d.process_exclusive.is_none());
    assert!(d.takes_recon_trigger, "the resync supervisor pokes the reconcile driver");
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentity::Undeterminable {
            why: "the store holds an HMAC api key/secret pair and no account identifier; a bybit \
                  sub-account is selected BY THE KEY, which names it nowhere offline",
        }
    );
    assert_eq!(
        d.clock,
        ClockDecl::Wired {
            endpoint: "GET /v5/market/time (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::SignedTimestamp,
        }
    );
}

/// THE D1 MATRIX (docs/decisions/0095): the ceiling alone picks the key tier, and a `live` ceiling
/// without the LIVE pair is PAPER for that reason — it never falls back to the DEMO pair.
#[test]
fn resolve_reads_exactly_the_tier_the_ceiling_names() {
    let no_credentials = Resolution::Paper(PaperCause::NoCredentials);
    let no_live_credentials = Resolution::Paper(PaperCause::LiveCredentialsAbsent);
    let empty = MountFixture::new(&[]);
    let demo = keyed(&AccountLabel::Default, &[demo_pair()]);
    let live = keyed(&AccountLabel::Default, &[live_pair()]);
    assert_eq!(BybitVenueMount.resolve(&empty.inputs(false)), no_credentials);
    assert_eq!(BybitVenueMount.resolve(&empty.inputs(true)), no_live_credentials);
    assert_eq!(
        BybitVenueMount.resolve(&demo.inputs(false)),
        Resolution::Armed { tier: Tier::Demo, held_below_live: None }
    );
    assert_eq!(
        BybitVenueMount.resolve(&demo.inputs(true)),
        no_live_credentials,
        "a live ceiling never falls back to the demo pair"
    );
    assert_eq!(
        BybitVenueMount.resolve(&live.inputs(true)),
        Resolution::Armed { tier: Tier::Live, held_below_live: None }
    );
    assert_eq!(
        BybitVenueMount.resolve(&live.inputs(false)),
        no_credentials,
        "a lower ceiling never reads the live pair"
    );
}

/// Review Focus 2 at this venue: holding BOTH pairs, a ceiling below `live` never arms live.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let both = keyed(&AccountLabel::Default, &[demo_pair(), live_pair()]);
    assert!(!matches!(
        BybitVenueMount.resolve(&both.inputs(false)),
        Resolution::Armed { tier: Tier::Live, .. }
    ));
}

/// The account 2×2 — bybit is the venue `vike-mount`'s `bybit_store_with_alt` fixture was written
/// for: a sub-account is selected BY THE KEY, so ALT's keys are ALT's book, and no account arms off
/// another's pair.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default, &[demo_pair()]);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        BybitVenueMount.resolve(&alt_asks_default_keys.inputs(false)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default account's keys"
    );
    assert!(matches!(
        BybitVenueMount.resolve(&keyed(&alt(), &[demo_pair()]).inputs(false)),
        Resolution::Armed { .. }
    ));
    let mut default_asks_alt_keys = keyed(&alt(), &[demo_pair()]);
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        BybitVenueMount.resolve(&default_asks_alt_keys.inputs(false)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// No key set for the ceiling's tier: paper, nothing to reconcile, nothing recorded — and OFFLINE,
/// which the body shows when its three outward steps are doubles that panic: it returns before the
/// grid pre-fetch, the reconcile factory and the exec spawn.
///
/// ⚠ The doubled body runs FIRST, and the order is the point: were the credential gate ever to
/// regress, the row fails there, on an `unreachable!`, before the real `mount` below could run its
/// `instruments-info` pre-fetch and spawn an exec thread against a live host from a unit test.
#[test]
fn without_a_key_set_for_the_ceilings_tier_the_mount_is_paper_and_offline() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let empty = MountFixture::new(&[]);
    let demo_only = keyed(&AccountLabel::Default, &[demo_pair()]);
    let live_only = keyed(&AccountLabel::Default, &[live_pair()]);
    for (fx, ceiling_live) in
        [(&empty, false), (&empty, true), (&demo_only, true), (&live_only, false)]
    {
        let body = mount_with(
            fx.request(ceiling_live, "BTCUSDT", &tx),
            |_, _, _| unreachable!("no grid pre-fetch without a key set"),
            |_, _, _| unreachable!("no reconcile client without a key set"),
            |_| unreachable!("no exec spawn without a key set"),
        );
        assert!(matches!(body.exec, ExecOutcome::Paper), "ceiling_live={ceiling_live}");
        assert!(body.recon.is_none() && body.identity.is_none());
        let out = BybitVenueMount.mount(fx.request(ceiling_live, "BTCUSDT", &tx));
        assert!(matches!(out.exec, ExecOutcome::Paper), "ceiling_live={ceiling_live}");
        assert!(out.recon.is_none() && out.identity.is_none());
    }
    assert!(BybitVenueMount.credential_probe(&empty.inputs(true)).is_none());
}

/// Was `vike-mount`'s `credentialed_bybit_yields_a_recon_client` and the bybit line of its
/// `an_armed_mainnet_verdict_still_yields_a_recon_client`. Those asked the reconcile FACTORY alone,
/// which answers `Some` by construction, so no edit to the mount could turn them red. This drives
/// the mount's own body at both tiers: the reconcile client is built on the network the grid
/// pre-fetch and the exec spawn are handed — the one the ceiling names — and comes back beside the
/// exec client, which is bound at that tier and signs with that tier's pair even when the store
/// holds both.
#[test]
fn the_mount_reconciles_on_the_network_it_trades_on() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let both = vec![demo_pair(), live_pair()];
    for (sets, ceiling_live) in
        [(vec![demo_pair()], false), (vec![live_pair()], true), (both.clone(), false), (both, true)]
    {
        let fx = keyed(&AccountLabel::Default, &sets);
        let (mut grid_on, mut recon_on, mut spawned_on) = (None, None, None);
        let mut signed_with = None;
        let out = mount_with(
            fx.request(ceiling_live, "BTCUSDT", &tx),
            |_, _, mainnet| {
                grid_on = Some(mainnet);
                None
            },
            |c, symbol, mainnet| {
                recon_on = Some(mainnet);
                crate::recon_client::recon_client(c, symbol, mainnet)
            },
            |spawn| {
                spawned_on = Some(spawn.mainnet);
                signed_with = Some(spawn.creds.api_key);
                Box::new(Unspawned)
            },
        );
        let ExecOutcome::Live(exec) = out.exec else {
            panic!("a key set at the ceiling's tier mounts live (ceiling_live={ceiling_live})");
        };
        let tier = if ceiling_live { Tier::Live } else { Tier::Demo };
        assert_eq!(exec.bound_tier, tier, "the tier the mount binds");
        let on = Some(ceiling_live);
        assert_eq!((grid_on, recon_on, spawned_on), (on, on, on), "one network for all three");
        let key = if ceiling_live { "lk" } else { "dk" };
        assert_eq!(
            signed_with.as_deref(),
            Some(key),
            "the exec client signs with that tier's pair"
        );
        assert!(out.recon.is_some(), "the reconcile client comes back beside the exec client");
        assert!(out.identity.is_none());
    }
}

/// The exec client is spawned with what the legacy arm read for it: the FD-broker code and the
/// `execution.fast` hint out of the credential map, the leverage out of the operator's `[risk]`
/// budget, and the reconnect trigger the resync supervisor pokes — each ABSENT one reading as the
/// arm's default (no `X-Referer` header, the historical 2x, the fast stream off, no trigger).
#[test]
fn the_exec_client_is_spawned_with_what_the_arm_read() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let bare = keyed(&AccountLabel::Default, &[demo_pair()]);
    assert_eq!(spawned_with(bare.request(false, "BTCUSDT", &tx)), (None, false, 2.0, false));

    let mut set = keyed(&AccountLabel::Default, &[demo_pair()]);
    // The broker-code NAME is asked of the table module for the reason `live_pair` gives; for
    // bybit's `X-Referer` header mechanic it is the `_BROKER_CODE` spelling.
    let broker_code = attribution_var_for(VENUE).expect("bybit stamps an attribution code");
    set.vars.insert(broker_code, "VK42".to_string());
    // `venue.bybit.fast_exec` — a `venue_setting` row, which the mount reads from
    // `MountInputs::settings` (decision 0095, Task 7).
    set.settings = vike_secrets::venue_setting::VenueSettings::from_rows(
        VENUE,
        &[vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: None,
            field: "FAST_EXEC".to_string(),
            value: "1".to_string(),
        }],
    );
    let profile = vike_exec::ProfileRisk { max_leverage: Some(5.0), ..Default::default() };
    let (trigger, _poked) = mpsc::channel();
    let mut req = set.request(false, "BTCUSDT", &tx);
    req.risk_profile = Some(&profile);
    req.recon_trigger = Some(trigger);
    assert_eq!(spawned_with(req), (Some("VK42".to_string()), true, 5.0, true));
}

/// THE TIER at the one leg that SIGNS at startup, before any mount: with both pairs in the store,
/// the probe signs with the pair the ceiling names, on that tier's HOST, and reports that tier —
/// so a `demo` ceiling never sends a signed read to the real-money account. With only the other
/// tier's pair there is no probe at all. Pure — the reconcile client is a signer plus a transport.
#[test]
fn a_demo_ceiling_never_signs_against_the_real_money_tier() {
    let both = keyed(&AccountLabel::Default, &[demo_pair(), live_pair()]);
    for (ceiling_live, key, tier) in [(false, "dk", Tier::Demo), (true, "lk", Tier::Live)] {
        let mut signed = None;
        let probe = credential_probe_with(&both.inputs(ceiling_live), |c, symbol, mainnet| {
            signed = Some((c.api_key.clone(), symbol.to_string(), mainnet));
            crate::recon_client::recon_client(c, symbol, mainnet)
        });
        let Some(CredentialProbe::RecordsIdentity { bound_tier: reported, .. }) = probe else {
            panic!("a pair at the ceiling's tier yields an identity-recording probe");
        };
        assert_eq!(reported, tier, "the tier the probe reports (ceiling_live={ceiling_live})");
        assert_eq!(
            signed,
            Some((key.to_string(), PROBE_SYMBOL.to_string(), ceiling_live)),
            "the key the probe signs with and the host it signs against are that tier's"
        );
    }
    let live_only = keyed(&AccountLabel::Default, &[live_pair()]);
    assert!(BybitVenueMount.credential_probe(&live_only.inputs(false)).is_none());
    let demo_only = keyed(&AccountLabel::Default, &[demo_pair()]);
    assert!(BybitVenueMount.credential_probe(&demo_only.inputs(true)).is_none());
}

// ---- the clock PARSE, against the real captured body (moved from vike-mount) -------------------

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
