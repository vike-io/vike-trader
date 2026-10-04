//! The pre-connect live-intent probe and the arming ceiling, driven over the REAL registry with
//! real credential shapes. They lived in `crates/vike-mount/src/preconnect_tests.rs`, which keeps
//! the one test that names no venue row, until the venue mount contract finished
//! (docs/decisions/0096): each venue's probe is its bridge's `VenueMount::resolve` now, which only
//! the crate holding the registry can reach.

use std::collections::HashMap;

use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::registry::REGISTRY;

use crate::support::vars;

/// A bybit store holding the DEFAULT account's demo key set and a LABELLED `ALT` one beside it.
///
/// ⚠ The labelled key NAMES are rendered by `vike_model::accounts::account_keys::account_key` rather than
/// written out, and that is not style. `crates/vike-ops/tests/settings_registry.rs` harvests
/// credential-key LITERALS out of every `.rs` file under `crates/*/src/` and demands a
/// `vike_ops::settings::SETTINGS` row for each; a `__ALT` spelling has no row and can never have
/// one, because the label is a name the OPERATOR chooses at runtime. Building the key through
/// the grammar's own renderer keeps the literal out of the source AND makes this fixture wrong
/// the day the separator changes.
fn bybit_store_with_alt() -> HashMap<String, String> {
    use vike_model::accounts::account_keys::account_key;
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let mut out = HashMap::new();
    for (base, value) in [("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")] {
        out.insert(base.to_string(), value.to_string());
        out.insert(account_key(base, &alt), format!("{value}2"));
    }
    out
}

/// Roster-wide inert default (the capability-map playbook's completeness direction): with NO
/// credentials of any shape, EVERY roster venue probes paper — a mount with an empty vars map
/// must never be refused over a risk budget it cannot need.
#[test]
fn empty_vars_probe_false_for_every_roster_venue() {
    for v in vike_model::VENUES {
        assert!(
            !vike_mount::would_mount_live(REGISTRY, v, &HashMap::new()),
            "{v} must probe paper with no creds"
        );
    }
}

/// One `(venue, its live arm's own credential shape)` row per venue with a live arm in a
/// DEFAULT build — the var names are the arms' own loaders' names (fixture-pinned in each
/// bridge crate).
///
/// Hoisted out of `probe_recognizes_each_live_arms_cred_shape` because the arming-ceiling
/// suite below needs exactly the same maps for the opposite claim: those tests assert that a
/// `paper` ceiling refuses a mount these maps WOULD have armed, and a table of their own would
/// let the two drift until the ceiling suite was silently testing unarmed venues.
/// `every_roster_venue_is_a_live_arm_row_or_has_no_live_arm` is the completeness half.
///
/// ⚠ **binance/bybit/okx/hyperliquid carry LIVE-tier keys, not DEMO-tier — decision 0095.**
/// `would_mount_live` probes at the `Live` ceiling, and those four venues now REQUIRE LIVE-tier
/// credentials to reach anything but Paper under it (a mainnet host is never signed with demo
/// keys), so a DEMO-shaped row here would probe false and this table would stop being "the shape
/// that arms this venue's live arm" for exactly the venues the migration changed. This table said
/// DEMO for all four until that regression was measured.
fn live_arming_cases() -> &'static [(&'static str, &'static [(&'static str, &'static str)])] {
    &[
        ("binance", &[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")]),
        ("bybit", &[("BYBIT_LIVE_API_KEY", "k"), ("BYBIT_LIVE_API_SECRET", "s")]),
        (
            "okx",
            &[
                ("OKX_LIVE_API_KEY", "k"),
                ("OKX_LIVE_API_SECRET", "s"),
                ("OKX_LIVE_API_PASSPHRASE", "p"),
            ],
        ),
        ("deribit", &[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")]),
        ("aster", &[("ASTER_TESTNET_USER", "0xUser"), ("ASTER_TESTNET_PRIVATE_KEY", "0xkey")]),
        ("hyperliquid", &[("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xkey")]),
        (
            "ctrader",
            &[
                ("CTRADER_CLIENT_ID", "id"),
                ("CTRADER_CLIENT_SECRET", "sec"),
                ("CTRADER_DEMO_ACCESS_TOKEN", "at"),
                ("CTRADER_DEMO_REFRESH_TOKEN", "rt"),
            ],
        ),
        (
            "alpaca",
            &[
                ("ALPACA_SANDBOX_CLIENT_ID", "id"),
                ("ALPACA_SANDBOX_CLIENT_SECRET", "sec"),
                ("ALPACA_SANDBOX_ACCOUNT_ID", "acct"),
            ],
        ),
        ("ig", &[("IG_DEMO_API_KEY", "k"), ("IG_DEMO_IDENTIFIER", "u"), ("IG_DEMO_PASSWORD", "p")]),
        ("oanda", &[("OANDA_DEMO_API_KEY", "k"), ("OANDA_DEMO_ACCOUNT_ID", "a")]),
        // ⚠ A BESPOKE SHAPE, not the `{VENUE}_{TIER}_API_KEY/_SECRET` grid: JForex authenticates
        // with the same LOGIN and PASSWORD as the desktop platform, so there is no API key to
        // name. `SERVER` is absent here — `load_dukascopy_config_from` gates on login and
        // password alone, and the server is not a credential at all since decision 0095's Task 7
        // (it is the `venue.dukascopy.demo.server` setting).
        //
        // This row is what removing dukascopy from `NO_LIVE_ARM` obliges: the venue now HAS a
        // live arm, so the arming-ceiling suite must actually try to refuse it. That is the
        // half that matters — a venue with no row is a venue the `paper` ceiling is never
        // tested against.
        ("dukascopy", &[("DUKASCOPY_DEMO1_LOGIN", "u"), ("DUKASCOPY_DEMO1_PASSWORD", "p")]),
    ]
}

/// Each live arm's credential shape flips its probe row to `true` — so a loader renaming its
/// keys, or this probe drifting off its arm's loader, fails by name.
#[test]
fn probe_recognizes_each_live_arms_cred_shape() {
    for (venue, kv) in live_arming_cases() {
        assert!(
            vike_mount::would_mount_live(REGISTRY, venue, &vars(kv)),
            "{venue}: its live arm's cred shape must probe live"
        );
    }
}

/// The venues [`live_arming_cases`] deliberately has NO row for, each with the reason — the
/// not-applicable column of the same table, which is what makes the partition below a
/// completeness gate rather than a length check.
///
/// Every one of them is a venue no DEFAULT build can mount live from a credential map alone, so
/// there is no map on which `would_mount_live` is true and nothing for the arming-ceiling suite
/// to refuse. `venues_without_a_live_arm_probe_false_even_with_creds` and
/// `fxcm_probes_false_with_the_feature_off_even_with_creds` drive the individual cases with their
/// real key shapes. fxcm's feature-on half runs in `crates/vike-tradehub/tests/fxcm_mount.rs`,
/// ibkr's in `crates/vike-tradehub/tests/ibkr_mount.rs` and polymarket's in
/// `crates/vike-tradehub/tests/polymarket_mount.rs`, since their features moved from vike-mount to
/// vike-tradehub.
const NO_LIVE_ARM: &[(&str, &str)] = &[
    (
        "fxcm",
        "the default build's registry carries it FeatureAbsent: its mount is the fxcm bridge's, \
         registered only under vike-tradehub's `fxcm` feature, and it needs a loadable \
         ForexConnect shim",
    ),
    (
        "ibkr",
        "the default build's registry carries it FeatureAbsent: its mount is the ibkr bridge's, \
         registered only under vike-tradehub's `ibkr` feature",
    ),
    (
        "polymarket",
        "the default build's registry carries it FeatureAbsent: its mount is the polymarket \
         bridge's, registered only under vike-tradehub's `polymarket` feature, and it needs \
         flags.poly_exec and a live ceiling",
    ),
];

/// **The COMPLETENESS gate over [`live_arming_cases`]**, in the shape every per-venue
/// capability table uses: the armed rows and the declared not-applicable rows must partition
/// `vike_model::VENUES` exactly — no venue in both, no venue in neither.
///
/// A thirteenth bridge therefore reddens this until its author says which column it is in, and
/// a venue that GAINS a live arm cannot stay silently uncovered by the arming-ceiling suite
/// below (the failure that matters: that suite proves a `paper` ceiling refuses an arming, and
/// a venue with no row is a venue it never tries to refuse).
#[test]
fn every_roster_venue_is_a_live_arm_row_or_a_declared_not_applicable() {
    let mut rows: Vec<&str> = live_arming_cases().iter().map(|(v, _)| *v).collect();
    let excused: Vec<&str> = NO_LIVE_ARM.iter().map(|(v, _)| *v).collect();
    for (venue, why) in NO_LIVE_ARM {
        assert!(why.len() > 20, "{venue}'s exemption must carry a REASON, got {why:?}");
        assert!(!rows.contains(venue), "{venue} is in BOTH columns");
    }
    rows.extend(&excused);
    rows.sort_unstable();
    let mut roster = vike_model::VENUES.to_vec();
    roster.sort_unstable();
    assert_eq!(
        rows, roster,
        "`live_arming_cases` + NO_LIVE_ARM must be exactly the roster: a new venue needs its \
             live arm's credential shape in the first, or a written reason in the second"
    );
    // vike:new-venue:note add "{venue}" to `NO_LIVE_ARM` with the reason its scaffolded mount cannot arm (its `resolve` answers `NoLiveArm`), and move it into `live_arming_cases` with its real credential shape in the PR that makes that mount arm — this partition is exact over the roster, so it stays red until the venue is in one of the two: crates/vike-tradehub/tests/mount_roster/preconnect.rs's `every_roster_venue_is_a_live_arm_row_or_a_declared_not_applicable`
}

/// **THE HALF-CREDENTIAL GATE, at the mount.** OKX's signer sends `OK-ACCESS-PASSPHRASE` on
/// every request (`vike_bridge_core::venue_passphrase`'s `venue_passphrase` row), so a store
/// holding key + secret and NO `OKX_{TIER}_API_PASSPHRASE` is UNUSABLE — and until this gate it
/// LOADED: `("okx", Some(c))` matched, the venue was marked live, an exec actor was spawned,
/// and every signed request came back rejected by the venue. Half credentials must reach the
/// same verdict as absent ones, because absent credentials ARE the live gate.
///
/// Asserted at BOTH gates that decide it, since either alone could regress independently: the
/// pure pre-connect probe, and a REAL `make_engine` mount (which stays offline precisely
/// because the gate holds — a regression makes this test dial OKX).
#[test]
fn okx_key_and_secret_without_a_passphrase_do_not_mount_live() {
    // Decision 0095: LIVE-tier, not DEMO-tier — `would_mount_live` probes the `Live` ceiling, and
    // okx now requires LIVE-tier keys specifically to reach anything but Paper under it. Using
    // DEMO-tier keys here would make the tier itself the gate this test measures, not the
    // passphrase.
    let half = vars(&[("OKX_LIVE_API_KEY", "k"), ("OKX_LIVE_API_SECRET", "s")]);
    assert!(
        !vike_mount::would_mount_live(REGISTRY, "okx", &half),
        "okx with no passphrase must probe PAPER — its key+secret cannot sign anything"
    );

    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let (engine, recon) = vike_mount::make_engine(
        REGISTRY,
        "okx",
        "BTC-USDT-SWAP",
        &half,
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        // ⚠ okx is ARMED by the ceiling: the gate under test is the missing PASSPHRASE, and a
        // `paper` ceiling would produce the same paper mount one step earlier — making every
        // assertion below true for the wrong reason. `Live`, not `Demo` (decision 0095): with
        // LIVE-tier `half`, only a `live` ceiling is the meaningful "armed but for the passphrase"
        // case.
        Some(&crate::support::armed_policy("okx", vike_config::VenueMode::Live)),
    )
    .expect("a paper mount must never refuse to start");
    assert!(live.is_empty(), "half credentials must NOT mark okx live");
    assert!(recon.is_none(), "a paper venue never reconciles");
    assert_eq!(
        engine.fee_schedule,
        Some(vike_model::fee_schedule_for("okx")),
        "the paper mount is tagged with okx's static fee schedule"
    );

    // The CONTROL: the same map plus the passphrase flips the probe live, so this test pins the
    // PASSPHRASE as the gate and not some unrelated okx breakage. (Only the pure probe is
    // exercised on that side — `make_engine` with complete credentials would dial the venue.)
    let mut full = half.clone();
    full.insert("OKX_LIVE_API_PASSPHRASE".to_string(), "p".to_string());
    assert!(
        vike_mount::would_mount_live(REGISTRY, "okx", &full),
        "with all three credentials okx probes live — the gate is the passphrase, nothing else"
    );
}

/// **THE OANDA LIVE-TIER REFUSAL, at the mount.** `vike_oanda::oanda_hosts` implements and
/// tests the fxTrade tier and no caller in this workspace ever asks for it, so a store holding
/// `OANDA_LIVE_*` used to mount PAPER in silence — and a store holding BOTH tiers used to place
/// REAL orders on the practice account while the operator believed their live keys were in
/// force. The arm now refuses to select ANY tier from a live-armed store, the practice fallback
/// included, and says so at `error!`.
///
/// Asserted at BOTH gates that decide it, the same shape as the OKX passphrase gate above: the
/// pure pre-connect probe, and a REAL `make_engine` mount — which stays offline precisely
/// because the refusal holds. A regression makes this test dial OANDA's practice account with
/// the demo token below, which is the behaviour being pinned out.
#[test]
fn an_oanda_live_key_set_refuses_the_mount_instead_of_trading_the_practice_account() {
    let armed = vars(&[
        ("OANDA_LIVE_API_KEY", "live-tok"),
        ("OANDA_LIVE_ACCOUNT_ID", "001-001-0000001-001"),
        ("OANDA_DEMO_API_KEY", "demo-tok"),
        ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
    ]);
    assert!(
        !vike_mount::would_mount_live(REGISTRY, "oanda", &armed),
        "a live-armed oanda store must probe PAPER — the arm refuses it, so live INTENT here \
             would raise a budget refusal over a venue that can only be paper"
    );

    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    // ⚠ oanda is ARMED by the ceiling: the gate under test is the arm's own refusal of a
    // LIVE-named key set, and a `paper` ceiling would reach the same paper mount one step
    // earlier — making every assertion below true for the wrong reason.
    let permitted = crate::support::armed_policy("oanda", vike_config::VenueMode::Demo);
    let (engine, recon) = vike_mount::make_engine(
        REGISTRY,
        "oanda",
        "EUR_USD",
        &armed,
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        Some(&permitted),
    )
    .expect("a paper mount must never refuse to start");
    assert!(live.is_empty(), "a live-armed oanda store must NOT mount live on practice");
    assert!(recon.is_none(), "a paper venue never reconciles");
    assert_eq!(
        engine.fee_schedule,
        Some(vike_model::fee_schedule_for("oanda")),
        "the paper mount is tagged with oanda's static fee schedule"
    );

    // The CONTROL: the SAME map with the live pair removed probes live again, so this test
    // pins the live-named key set as the gate and not some unrelated oanda breakage. (Only the
    // pure probe is exercised on that side — `make_engine` with practice credentials would
    // dial the venue.)
    let practice = vars(&[
        ("OANDA_DEMO_API_KEY", "demo-tok"),
        ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
    ]);
    assert!(
        vike_mount::would_mount_live(REGISTRY, "oanda", &practice),
        "practice credentials alone still probe live — the gate is the live-named set, \
             nothing else"
    );
}

/// The MIRROR of the gate above, and the reason it is a per-venue TABLE rather than a blanket
/// rule: binance/bybit/deribit sign with key + secret alone, so requiring a passphrase
/// everywhere would silently strand every working mount on paper — the same defect wearing the
/// opposite sign. Their probes must stay live with exactly two credentials.
#[test]
fn passphrase_free_venues_still_probe_live_without_one() {
    for (venue, kv) in [
        // Decision 0095: binance/bybit need LIVE-tier keys to probe live under `would_mount_live`
        // (the `Live`-ceiling probe) — deribit's network is never the ceiling, so it stays DEMO.
        ("binance", &[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")]),
        ("bybit", &[("BYBIT_LIVE_API_KEY", "k"), ("BYBIT_LIVE_API_SECRET", "s")]),
        ("deribit", &[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")]),
    ] {
        assert!(
            vike_mount::would_mount_live(REGISTRY, venue, &vars(kv)),
            "{venue} takes no passphrase — key+secret alone must still probe live"
        );
    }
}

/// FXCM credentials for the probe test below — the `load_fxcm_config_from(Demo, …)` shape the fxcm
/// mount gates on.
fn fxcm_creds() -> HashMap<String, String> {
    vars(&[("FXCM_DEMO_USER", "D251112911"), ("FXCM_DEMO_PASSWORD", "p")])
}

/// This crate's registry carries fxcm `FeatureAbsent` in every build since its bridge left with the
/// `fxcm` feature, so it probes false with credentials present — the answer a build without the
/// feature has always given. The feature-on half is `crates/vike-tradehub/tests/fxcm_mount.rs`'s.
#[test]
fn fxcm_probes_false_with_the_feature_off_even_with_creds() {
    assert!(
        !vike_mount::would_mount_live(REGISTRY, "fxcm", &fxcm_creds()),
        "no feature ⇒ no arm ⇒ paper, whatever the store says"
    );
}

/// Venues whose registry row cannot arm live probe paper even when their own credential shapes
/// are present, so a budget refusal over one would block a mount that can only ever be paper.
///
/// ⚠ This doc named dukascopy — *"ships an exec factory, but nothing in `make_engine` mounts
/// it"* — and that has been false since 2026-09-09; the test body already said so and the
/// summary line above it did not. What remains here is polymarket as the default build's registry
/// carries it — `FeatureAbsent` — whose feature-on half is
/// `crates/vike-tradehub/tests/polymarket_mount.rs`'s
/// `the_exec_flag_alone_is_not_intent_and_the_flag_with_a_key_is`.
///
/// ⚠ fxcm USED TO BE IN THIS TEST and is not any more: it grew an arm. Its replacement is
/// `fxcm_probes_false_with_the_feature_off_even_with_creds` above, which asserts the same `false`
/// in the default build this module runs in, with the two-fact conjunction asserted in
/// `crates/vike-tradehub/tests/fxcm_mount.rs` and in the bridge's own
/// `crates/bridges/fxcm/src/mount_tests.rs`.
#[test]
fn venues_without_a_live_arm_probe_false_even_with_creds() {
    // ⚠ dukascopy LEFT this test on 2026-09-09, for the same reason fxcm did: it grew an arm.
    // It used to be the first assertion here, and `crates/vike-mount/src/lib.rs`'s own
    // `NO_LIVE_ARM` table carried its reason — "ships an exec factory, and `make_engine` has no
    // arm that mounts it". Both are gone; the venue now has a row in `live_arming_cases` and a
    // probe arm that self-gates on `load_dukascopy_config_from(Demo1, …)`, so a store carrying
    // its credentials probes TRUE and the arming-ceiling suite exercises the refusal.
    // polymarket is `FeatureAbsent` in the default build's registry, so neither map arms it; under
    // vike-tradehub's `polymarket` feature its mount needs BOTH gates: the explicit
    // flags.poly_exec flag AND key material. The flag ALONE can never mount live (no key ⇒ the
    // factory returns before any network) and must stay paper-probed — the contract
    // `crates/vike-tradehub/tests/polymarket_mount.rs`'s
    // `polymarket_with_the_exec_gate_but_no_creds_stays_paper_and_offline` pins.
    assert!(!vike_mount::would_mount_live(REGISTRY, "polymarket", &vars(&[("POLY_EXEC", "1")])));
    assert!(!vike_mount::would_mount_live(
        REGISTRY,
        "polymarket",
        &vars(&[("POLY_EXEC", "1"), ("POLY_PRIVATE_KEY", "0xkey")])
    ));
}

// ===========================================================================================
// THE ARMING CEILING (settings-unification stage 3)
//
// ⚠ Every test below drives the REAL `make_engine_with_legs` with REAL live-arming credential
// shapes, and every one of them stays OFFLINE — because the ceiling holds. A regression makes
// this suite dial ten venues, which is the same stance (and the same wording) as the OKX
// passphrase and OANDA live-tier gates above.
//
// ⚠ The LIVE side is asserted through the pre-connect BUDGET REFUSAL rather than through
// `live_venues`, and that is not a weaker check dressed up — it is the only observable a
// network-free test HAS. Reaching `live_venues.insert` means the venue's arm ran, i.e. a
// blocking instrument fetch and a dialing exec thread; `MountError::MissingRiskBudget` fires
// one step earlier, from `would_mount_live_under`, and firing it PROVES the mount classified
// the venue as live-intent under that ceiling. The paper side, where nothing dials, is asserted
// on `live_venues` directly.
// ===========================================================================================

use vike_config::{VenueMode, VenuePolicy};

/// A `MountPolicy` declaring exactly one venue's ceiling — every other venue keeps `paper`.
fn ceiling(venue: &str, mode: VenueMode) -> vike_mount::MountPolicy {
    vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare(venue, mode),
        ..vike_mount::MountPolicy::default()
    }
}

/// Mount `venue` for real, with no risk profile and no recorder, and report both halves: the
/// result and the live set the mount wrote into.
fn mount(
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&vike_mount::MountPolicy>,
) -> (Result<vike_mount::EngineAndRecon, vike_mount::MountError>, std::collections::HashSet<String>)
{
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = std::collections::HashSet::new();
    let out = vike_mount::make_engine_with_legs(
        REGISTRY,
        venue,
        "BTCUSDT",
        &[],
        vars,
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        policy,
    );
    (out, live)
}

/// **THE STAGE-3 PROPERTY, over the WHOLE roster**: a venue whose ceiling is `paper` never gets
/// a live exec client, no matter what its credentials say — and the very same credentials under
/// a `live` ceiling do reach the live path, so the refusal is the ceiling and not a broken
/// fixture.
///
/// ⚠ The `None`-policy leg is not redundant with the `MountPolicy::default()` one, and it is
/// the leg that catches the fail-safe default being weakened: `policy.map_or(Paper, …)` and
/// `policy.map_or(Live, …)` agree on every `Some`, and differ only here.
#[test]
fn a_paper_moded_venue_never_gets_a_live_exec_client() {
    for (venue, kv) in live_arming_cases() {
        let armed = vars(kv);
        // ANTI-VACUITY: these credentials really would arm this venue. Without this line every
        // assertion below could pass against an empty map.
        assert!(
            vike_mount::would_mount_live(REGISTRY, venue, &armed),
            "{venue}: the fixture must be a map that ARMS, or this test proves nothing"
        );

        let no_file = vike_mount::MountPolicy::default();
        for policy in [None, Some(&no_file)] {
            let (out, live) = mount(venue, &armed, policy);
            let (engine, recon) = out.unwrap_or_else(|e| {
                panic!("{venue}: a capped mount is PAPER and must never refuse to start: {e}")
            });
            assert!(
                live.is_empty(),
                "{venue}: a `paper` ceiling let a live exec client be armed (policy \
                     supplied: {})",
                policy.is_some()
            );
            assert!(recon.is_none(), "{venue}: a paper venue never gets a reconcile handle");
            assert_eq!(
                engine.fee_schedule,
                Some(vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, "BTCUSDT"))),
                "{venue}: the capped mount is still tagged with the venue's fee schedule"
            );
        }
        assert!(
            !vike_mount::would_mount_live_under(REGISTRY, venue, &armed, VenueMode::Paper),
            "{venue}: a disarmed venue has no live arm to probe"
        );

        // …and the CONTROL, which is what makes the refusals above the CEILING's doing: the
        // same map under `live` reaches the live path. Observed as the pre-connect budget
        // refusal — no venue session is established, so this stays offline.
        assert!(
            vike_mount::would_mount_live_under(REGISTRY, venue, &armed, VenueMode::Live),
            "{venue}: the same credentials under a `live` ceiling must probe live"
        );
        let permitted = ceiling(venue, VenueMode::Live);
        match mount(venue, &armed, Some(&permitted)).0 {
            Err(vike_mount::MountError::MissingRiskBudget { venue: refused, .. }) => {
                assert_eq!(refused.as_str(), *venue, "the refusal must name the venue")
            }
            Ok(_) => panic!(
                "{venue}: a `live` ceiling over live credentials did NOT reach the live path — \
                     the pre-connect budget refusal never fired, so the ceiling is refusing \
                     everything and the paper assertions above prove nothing"
            ),
        }

        // EXACTLY that venue: the same one-venue `live` policy leaves every OTHER armed venue
        // on paper, so a ceiling is per-venue rather than a global switch.
        for (other, other_kv) in live_arming_cases() {
            if other == venue {
                continue;
            }
            let (out, live) = mount(other, &vars(other_kv), Some(&permitted));
            out.unwrap_or_else(|e| panic!("{other} is capped and must not refuse: {e}"));
            assert!(live.is_empty(), "{venue}=live must not arm {other}");
        }
    }
}

/// **`demo` NEVER REACHES THE LIVE KEY SET.** Decision 0095: under a `demo` ceiling the DEMO tier
/// is resolved regardless of which keys are present, so LIVE keys alone, with no demo keys, leave
/// the venue on PAPER — never a mainnet host signed with the live key set a `demo` ceiling
/// declined to use.
#[test]
fn a_demo_ceiling_never_reaches_the_live_key_set() {
    let armed = vars(&[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")]);
    // CONTROL FIRST, so the refusal below cannot be a fixture that never armed anything: under
    // `live` this exact map reaches the live path (the pre-connect budget refusal fires).
    assert!(vike_mount::would_mount_live_under(REGISTRY, "binance", &armed, VenueMode::Live));
    assert!(matches!(
        mount("binance", &armed, Some(&ceiling("binance", VenueMode::Live))).0,
        Err(vike_mount::MountError::MissingRiskBudget { .. })
    ));

    // …and under `demo` the same map arms nothing at all.
    assert!(
        !vike_mount::would_mount_live_under(REGISTRY, "binance", &armed, VenueMode::Demo),
        "a `demo` ceiling resolves the DEMO tier, and there are no demo keys here"
    );
    let (out, live) = mount("binance", &armed, Some(&ceiling("binance", VenueMode::Demo)));
    out.expect("a demo-capped venue with only LIVE keys is PAPER, and paper never refuses");
    assert!(live.is_empty(), "the LIVE key set armed a venue the ceiling capped at `demo`");
}

/// **THE MEASURED HOLE.** Aster has never had a `{VENUE}_MAINNET`-shaped switch, so there is no
/// `ASTER_MAINNET` for `vike_config::CREDENTIAL_FILE_ARMING_REFUSED` to carry a row for, and its
/// arm tried `Environment::Live` FIRST. An `ASTER_LIVE_*` pair in
/// `secrets.env` was therefore by itself an authenticated MAINNET session on a daemon that
/// never asked for one (observed on the CI box, inside a nine-venue live set).
///
/// Under anything below `live` the Live attempt is DELETED from the chain, so the pair resolves
/// nothing and the venue is paper.
#[test]
fn demo_mode_deletes_the_live_first_attempt_on_a_switchless_venue() {
    let armed = vars(&[("ASTER_LIVE_USER", "0xUser"), ("ASTER_LIVE_PRIVATE_KEY", "0xkey")]);
    // CONTROL: the pair genuinely arms this venue under `live` — mainnet, first attempt, today.
    assert!(vike_mount::would_mount_live_under(REGISTRY, "aster", &armed, VenueMode::Live));
    assert!(matches!(
        mount("aster", &armed, Some(&ceiling("aster", VenueMode::Live))).0,
        Err(vike_mount::MountError::MissingRiskBudget { .. })
    ));

    for capped in [VenueMode::Demo, VenueMode::Paper] {
        assert!(
            !vike_mount::would_mount_live_under(REGISTRY, "aster", &armed, capped),
            "{capped}: LIVE aster credentials must arm nothing — there is no flag to refuse \
                 them, so the ceiling is the only refusal that exists"
        );
        let (out, live) = mount("aster", &armed, Some(&ceiling("aster", capped)));
        out.expect("a capped aster mount is PAPER, and paper never refuses to start");
        assert!(live.is_empty(), "{capped}: ASTER_LIVE_* armed a real mainnet account");
    }

    // …and the TESTNET pair still arms under `demo`, so what the ceiling deleted is the LIVE
    // attempt and not the venue.
    let testnet = vars(&[("ASTER_TESTNET_USER", "0xUser"), ("ASTER_TESTNET_PRIVATE_KEY", "0xkey")]);
    assert!(vike_mount::would_mount_live_under(REGISTRY, "aster", &testnet, VenueMode::Demo));
}

/// A capped venue must be the SAME engine an uncredentialled one gets — the ceiling refuses an
/// arming, it does not invent a third mode.
///
/// This is the guard on `paper_engine` being a separate assembly from the post-match tail: the
/// two are compared by OUTPUT (`vike_exec::state_hash` over the real snapshot, plus the fee
/// schedule, which the snapshot does not carry), so a step added to the tail and not here is
/// caught rather than merely commented about.
#[test]
fn a_capped_venue_is_the_same_engine_an_uncredentialled_one_gets() {
    for (venue, kv) in live_arming_cases() {
        let capped = mount(venue, &vars(kv), Some(&vike_mount::MountPolicy::default()))
            .0
            .unwrap_or_else(|e| panic!("{venue} capped: {e}"))
            .0;
        // The SAME venue with an EMPTY credential map, and a ceiling that permits everything —
        // so the only reason it is paper is the pre-existing absent-credentials gate.
        let unarmed = mount(venue, &HashMap::new(), Some(&ceiling(venue, VenueMode::Live)))
            .0
            .unwrap_or_else(|e| panic!("{venue} uncredentialled: {e}"))
            .0;
        assert_eq!(
            vike_exec::state_hash(&[capped.snapshot_state()]),
            vike_exec::state_hash(&[unarmed.snapshot_state()]),
            "{venue}: the capped mount and the uncredentialled one are different engines"
        );
        assert_eq!(capped.fee_schedule, unarmed.fee_schedule, "{venue}");
    }
}

/// **The regression this D1 fix exists for.** Until 2026-09-27 `paper_engine` took no `account`
/// and so could not read `policy.account_exposure.<venue>.<LABEL>` — the ONE limits line the
/// same-engine test above cannot catch, because neither `MountPolicy` it builds sets that table.
/// This plants a figure for the DEFAULT account (reachable on a single-account box, per
/// `vike_config::VenuePolicy::account_exposure`'s own doc) and asserts BOTH paper routes —
/// capped by the ceiling, and uncredentialled — apply it, mirroring the live twin's own fold in
/// `make_engine_for_account`'s post-match tail.
#[test]
fn a_paper_engine_applies_the_per_account_exposure_ceiling() {
    for (venue, kv) in live_arming_cases() {
        let cap = 42_000.0;
        let venues = VenuePolicy::default()
            .declare(venue, VenueMode::Paper)
            .declare_account_exposure(venue, &AccountLabel::Default, cap);
        let capped_policy =
            vike_mount::MountPolicy { venues, ..vike_mount::MountPolicy::default() };
        let capped = mount(venue, &vars(kv), Some(&capped_policy))
            .0
            .unwrap_or_else(|e| panic!("{venue} capped: {e}"))
            .0;
        assert_eq!(
            capped.gate.limits.max_account_exposure,
            Some(cap),
            "{venue}: a capped-paper engine must apply policy.account_exposure for the DEFAULT \
                 account, exactly as the live twin's post-match tail does"
        );

        let venues_unarmed = VenuePolicy::default()
            .declare(venue, VenueMode::Live)
            .declare_account_exposure(venue, &AccountLabel::Default, cap);
        let unarmed_policy = vike_mount::MountPolicy {
            venues: venues_unarmed,
            ..vike_mount::MountPolicy::default()
        };
        let unarmed = mount(venue, &HashMap::new(), Some(&unarmed_policy))
            .0
            .unwrap_or_else(|e| panic!("{venue} uncredentialled: {e}"))
            .0;
        assert_eq!(
            unarmed.gate.limits.max_account_exposure,
            Some(cap),
            "{venue}: an uncredentialled-paper engine must apply the SAME ceiling — a paper \
                 mount must never be the more permissive side, whichever reason made it paper"
        );
    }
}

// -- the migration warning ------------------------------------------------------------------

/// **The upgrade warning fires exactly once per box that needs it, and is silent otherwise.**
///
/// Four cases, and the third is the one that matters: an operator who wrote `[venues]` with
/// EVERY venue at `paper` has stated their arming, and a warning that keeps firing at them is
/// the "refusal list that fires on harmless lines" `vike_config::arming` names as the way
/// operators are taught to work around a check.
#[test]
fn the_migration_warning_fires_only_with_credentials_and_no_table() {
    let armed = vars(&[("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")]);

    // 1. credentials + no policy threaded at all → the warning.
    let msg = vike_mount::venue_arming_migration_message(REGISTRY, &armed, None)
        .expect("credentials and no `[venues]` table is exactly the upgrade case");
    // 2. …and the same for a real, loaded policy that simply never named a venue.
    assert_eq!(
        vike_mount::venue_arming_migration_message(
            REGISTRY,
            &armed,
            Some(&vike_mount::MountPolicy::default())
        ),
        Some(msg.clone()),
        "an undeclared policy is the same case as no policy"
    );

    // 3. SELF-SILENCING: an ALL-PAPER table produces the identical ceiling and silences it.
    let all_paper = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("bybit", VenueMode::Paper),
        ..vike_mount::MountPolicy::default()
    };
    assert_eq!(
        all_paper.venue_mode("bybit"),
        VenueMode::Paper,
        "the fixture must leave the CEILING unchanged, or it is silencing by widening"
    );
    assert_eq!(
        vike_mount::venue_arming_migration_message(REGISTRY, &armed, Some(&all_paper)),
        None,
        "a stated all-paper arming is a decision; warning at it trains people to ignore it"
    );

    // 4. No credentials → nothing was refused and there is nothing to paste.
    assert_eq!(vike_mount::venue_arming_migration_message(REGISTRY, &HashMap::new(), None), None);
}

/// The message must be ACTIONABLE and must leak nothing: the key, the venues that were refused,
/// a paste-ready `vike-cli config set` line per venue — and never a credential key NAME, let
/// alone a value. Same rule as `vike_config::refuse_credential_file_arming`, whose refusal
/// deliberately echoes no value from the credential store.
#[test]
fn the_migration_warning_names_the_command_and_only_venue_slugs() {
    let armed = vars(&[
        ("BYBIT_DEMO_API_KEY", "super-secret-key"),
        ("BYBIT_DEMO_API_SECRET", "super-secret-secret"),
        ("OANDA_DEMO_API_KEY", "another-secret"),
        ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
    ]);
    let msg = vike_mount::venue_arming_migration_message(REGISTRY, &armed, None)
        .expect("the upgrade case");

    assert!(!msg.contains("settings/policy.toml"), "names no FILE any more: {msg}");
    assert!(msg.contains("policy.venues"), "names the ROW space: {msg}");
    assert!(
        // Decision 0095: bybit's stored keys are DEMO-only, and bybit is one of the four venues
        // whose `live` ceiling now means MAINNET — so the paste-ready remedy names `demo`, the
        // tier these keys actually reach, never `live` (which would be a no-op paste).
        msg.contains("vike-cli config set policy.venues.bybit demo"),
        "the line is paste-ready and names the tier these keys actually reach: {msg}"
    );
    assert!(
        msg.contains("vike-cli config set policy.venues.oanda live"),
        "…for EVERY refused venue: {msg}"
    );
    assert!(!msg.contains("binance"), "a venue with no credentials is not in the list: {msg}");
    assert!(msg.contains("paper"), "says what the box is doing right now: {msg}");

    for (key, value) in armed.iter() {
        assert!(!msg.contains(key.as_str()), "leaked a credential key NAME ({key}): {msg}");
        assert!(!msg.contains(value.as_str()), "leaked a credential VALUE: {msg}");
    }
}

/// **THE SAFETY CASE, driven directly: a box holding BOTH tiers for a switched venue must still
/// be told to restore `demo`, never `live`.**
///
/// A box that reaches this message today could never have had `BYBIT_MAINNET` set — a SET one
/// refuses this process's own boot (`vike_config::REMOVED_ENV`), before any code here runs. So
/// under the OLD, pre-ceiling behaviour bybit NEVER resolved mainnet without that flag: whatever
/// LIVE-tier keys additionally sit in the store, this box's actual PRIOR trading was DEMO,
/// unconditionally. Before this test existed, the remedy line asked
/// `would_mount_live_under(venue, vars, Live)` — true the moment LIVE-tier keys are present,
/// regardless of whether they were ever reachable — so a dual-keyed box's "restore" line pasted
/// `vike-cli config set policy.venues.bybit live`, and an operator who ran it literally would have
/// moved a venue that had never traded anything but demo onto MAINNET. That is the exact failure
/// this task exists to prevent, reached through this task's own operator-facing remedy text.
#[test]
fn the_migration_warning_never_pastes_live_for_a_switched_venue_holding_both_tiers() {
    let dual_keyed = vars(&[
        ("BYBIT_DEMO_API_KEY", "demo-key"),
        ("BYBIT_DEMO_API_SECRET", "demo-secret"),
        ("BYBIT_LIVE_API_KEY", "live-key"),
        ("BYBIT_LIVE_API_SECRET", "live-secret"),
    ]);
    // ANTI-VACUITY: the fixture must actually reach LIVE tier under a `live` ceiling, or this
    // test would pass by accident — the real mount, not the message, is the independent witness.
    assert!(
        vike_mount::would_mount_live_under(REGISTRY, "bybit", &dual_keyed, VenueMode::Live),
        "the fixture must hold LIVE-tier keys that actually reach mainnet under a live ceiling, \
         or this test proves nothing about the bug it exists to catch"
    );

    let msg = vike_mount::venue_arming_migration_message(REGISTRY, &dual_keyed, None)
        .expect("the upgrade case");

    assert!(
        msg.contains("vike-cli config set policy.venues.bybit demo"),
        "a dual-keyed switched venue's restore line must name `demo` — its structurally-only-ever \
         prior behaviour — never `live`: {msg}"
    );
    assert!(
        !msg.contains("vike-cli config set policy.venues.bybit live"),
        "must NEVER paste `live` for a switched venue in the restore block: an operator who ran \
         this literally would move a venue that had only ever traded demo onto MAINNET: {msg}"
    );
    // …and the framing sentence must not claim the ceiling "can only ever REFUSE" — false for the
    // four switched venues, whose ceiling now SELECTS the network outright.
    assert!(
        msg.contains("SELECTS the tier"),
        "the framing must say the ceiling selects the network for the four switched venues, not \
         merely refuses: {msg}"
    );
}

/// **The same safety case for EVERY switched venue**, not bybit alone: each of the four venues
/// whose ceiling selects the network (decision 0095), on a box holding BOTH its tiers' keys, gets a
/// restore line of exactly `demo`. The rule is a membership test in `vike-mount`
/// (`crates/vike-mount/src/paper_fallback.rs`'s `network_is_the_ceiling`), so a change that kept
/// bybit's line right and got another venue's wrong would pass the test above.
///
/// The fixture list is held equal to `vike_secrets::live_means_mainnet::SWITCHED_VENUES`, so a
/// venue joining that list without a fixture here fails this test rather than going unchecked.
#[test]
fn the_migration_warning_pastes_demo_for_every_switched_venue_holding_both_tiers() {
    let dual_keyed: [(&str, &[(&str, &str)]); 4] = [
        (
            "binance",
            &[
                ("BINANCE_DEMO_API_KEY", "demo-key"),
                ("BINANCE_DEMO_API_SECRET", "demo-secret"),
                ("BINANCE_LIVE_API_KEY", "live-key"),
                ("BINANCE_LIVE_API_SECRET", "live-secret"),
            ],
        ),
        (
            "bybit",
            &[
                ("BYBIT_DEMO_API_KEY", "demo-key"),
                ("BYBIT_DEMO_API_SECRET", "demo-secret"),
                ("BYBIT_LIVE_API_KEY", "live-key"),
                ("BYBIT_LIVE_API_SECRET", "live-secret"),
            ],
        ),
        (
            "okx",
            &[
                ("OKX_DEMO_API_KEY", "demo-key"),
                ("OKX_DEMO_API_SECRET", "demo-secret"),
                ("OKX_DEMO_API_PASSPHRASE", "demo-pass"),
                ("OKX_LIVE_API_KEY", "live-key"),
                ("OKX_LIVE_API_SECRET", "live-secret"),
                ("OKX_LIVE_API_PASSPHRASE", "live-pass"),
            ],
        ),
        (
            "hyperliquid",
            &[
                ("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xdemo"),
                ("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xlive"),
            ],
        ),
    ];
    let mut covered: Vec<&str> = dual_keyed.iter().map(|(venue, _)| *venue).collect();
    covered.sort_unstable();
    let mut switched = vike_secrets::live_means_mainnet::SWITCHED_VENUES.to_vec();
    switched.sort_unstable();
    assert_eq!(covered, switched, "one dual-keyed fixture per switched venue");

    for (venue, keys) in dual_keyed {
        let vars = vars(keys);
        // ANTI-VACUITY, as above: the LIVE keys reach the live tier under a `live` ceiling, so a
        // restore line that asked the real probe would paste `live`.
        assert!(
            vike_mount::would_mount_live_under(REGISTRY, venue, &vars, VenueMode::Live),
            "{venue}: the fixture's LIVE-tier keys must reach the live tier under a live ceiling"
        );
        let msg = vike_mount::venue_arming_migration_message(REGISTRY, &vars, None)
            .unwrap_or_else(|| panic!("{venue}: the upgrade case"));
        let demo = format!("vike-cli config set policy.venues.{venue} demo");
        let live = format!("vike-cli config set policy.venues.{venue} live");
        assert!(
            msg.lines().any(|line| line == demo),
            "{venue}: the restore line must be exactly `{demo}`: {msg}"
        );
        assert!(
            !msg.lines().any(|line| line == live),
            "{venue}: must NEVER paste `live` for a switched venue holding both tiers: {msg}"
        );
    }
}

// ===========================================================================================
// THE ARMING PROJECTION (`venue_arming_under` / `venue_arming`) — the Data Manager's Venues
// tab reads its Effective column from here, so these gate that the column cannot disagree with
// the mount.
//
// ⚠ The agreement is asserted against `make_engine_with_legs` ITSELF wherever a network-free
// observable exists for it, not against a restatement of the rows: the tier a mount reaches is
// visible offline only as PAPER-vs-not (the pre-connect budget refusal fires from
// `would_mount_live_under`), so that half is driven through the real mount, and the
// Live-vs-Demo half is driven against the SAME per-venue tier resolvers the arms call.
// ===========================================================================================

/// **THE column's gate: for every roster venue, at every ceiling, over the real credential
/// fixtures, the projection's PAPER-vs-not verdict is what the mount actually does.**
///
/// ⚠ **What this proves, stated exactly.** The agreement is BY CONSTRUCTION — `make_engine`
/// consults `would_mount_live_under`, which is now `venue_arming_under` projected onto
/// paper-vs-not — so this test cannot catch the two functions disagreeing (they are one
/// function). What it DOES catch is the construction coming apart end-to-end: the mount
/// consulting a different ceiling than the one it was handed, the seam moving back below the
/// credential read, or the projection answering above its own ceiling. It drives the REAL
/// `make_engine_with_legs` with a REAL `MountPolicy` and reads its only offline observable, the
/// pre-connect budget refusal. The INDEPENDENT half — is the projected TIER the tier the arm
/// would dial — is
/// [`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`],
/// which pins, per switched venue and key set, the tier the projection reaches.
///
/// Anti-vacuity is built in two ways: the armed maps are asserted to arm SOMETHING at the live
/// ceiling before the comparison runs, and every venue is driven with an EMPTY map too, so a
/// projection that had degenerated into "always paper" would still have to agree with a mount
/// that has not.
#[test]
fn the_arming_projection_agrees_with_the_real_mount_for_every_roster_venue() {
    use vike_config::VenueMode as Mode;

    let armed_for: std::collections::HashMap<&str, HashMap<String, String>> =
        live_arming_cases().iter().map(|(v, kv)| (*v, vars(kv))).collect();
    // ANTI-VACUITY: at least one fixture really does arm, or every agreement below is between
    // two functions that both always say paper.
    assert!(
        armed_for.iter().any(|(v, map)| vike_mount::venue_arming_under(
            REGISTRY,
            v,
            map,
            Mode::Live
        )
        .0 != Mode::Paper),
        "no fixture arms anything — this test would prove nothing"
    );

    // ⚠ The THIRD map per venue, and it is the one that makes this test able to fail for the
    // venues it covers. Most `live_arming_cases` fixtures still arm at the DEMO tier, so for
    // those a `demo` ceiling leaves the capped and the UNCAPPED probes agreeing — a mount that
    // consulted `would_mount_live` instead of `would_mount_live_under` at its seam would sail
    // through them undetected. Decision 0095 moved binance/bybit/okx/hyperliquid's OWN rows in
    // `live_arming_cases` to LIVE-tier keys (their arms now require it — see that fn's doc), so
    // those four already create the divergence through `armed_for` alone; `mainnet_arming_cases`
    // is what still creates it for every OTHER row `live_arming_cases` cannot — aster above all,
    // whose `live_arming_cases` row is deliberately TESTNET-shaped.
    let mainnet_for: std::collections::HashMap<&str, HashMap<String, String>> =
        mainnet_arming_cases().iter().map(|(v, kv)| (*v, vars(kv))).collect();
    let empty = HashMap::new();
    for venue in vike_model::VENUES {
        for map in [
            armed_for.get(venue).unwrap_or(&empty),
            mainnet_for.get(venue).unwrap_or(&empty),
            &empty,
        ] {
            for cap in Mode::ALL {
                let (effective, block) = vike_mount::venue_arming_under(REGISTRY, venue, map, cap);

                // 1. A ceiling can only ever REFUSE. The screen renders `effective` beside
                //    `ceiling`, so a projection that promoted would render a lie.
                assert!(
                    effective <= cap,
                    "{venue} @ {cap}: projected {effective}, ABOVE the ceiling"
                );
                // 2. `block` and `effective` cannot contradict, in ONE direction: a CAPPED row
                //    must carry a reason, and a CLEAR block must be at its ceiling.
                //
                //    ⚠ Deliberately not an `assert_eq!` of the two. The converse is false and
                //    correctly so: a `paper` ceiling reports `Disarmed` and a mount-less build
                //    reports `NoMountInThisBuild` — both at their ceiling, both with something
                //    to say. A block is "what is holding this row where it is", which is
                //    information even when nothing is being refused.
                if effective < cap {
                    assert!(
                        !block.is_clear(),
                        "{venue} @ {cap}: capped to {effective} with NO reason — the Effective \
                             column would render a demotion the operator cannot explain"
                    );
                }
                if block.is_clear() {
                    assert_eq!(
                        effective, cap,
                        "{venue} @ {cap}: a clear block must mean the row is at its ceiling"
                    );
                }
                // 3. THE agreement, through the REAL mount.
                let policy = ceiling(venue, cap);
                let (out, live_set) = mount(venue, map, Some(&policy));
                let mount_is_live = match out {
                    Err(vike_mount::MountError::MissingRiskBudget { venue: named, .. }) => {
                        assert_eq!(named.as_str(), *venue, "the refusal must name the venue");
                        true
                    }
                    // ⚠ No catch-all `Err` arm: `MountError` has exactly one variant today, so
                    // one would be an unreachable pattern (`-D warnings`). A NEW variant makes
                    // this match non-exhaustive, which is a compile error naming this site —
                    // the right way round, since a new failure mode needs a decision here.
                    Ok(_) => {
                        assert!(
                            live_set.is_empty(),
                            "{venue} @ {cap}: a mount that armed live must have hit the \
                                 pre-connect budget refusal first"
                        );
                        false
                    }
                };
                assert_eq!(
                    effective != Mode::Paper,
                    mount_is_live,
                    "{venue} @ {cap}: the screen would say `{effective}` ({block:?}) while the \
                         mount {} — the Effective column exists precisely so these cannot differ",
                    if mount_is_live { "reaches its live arm" } else { "stays paper" }
                );
            }
        }
    }
}

/// The credential shapes that arm a venue at its **LIVE** tier — [`live_arming_cases`]'s
/// dangerous twin, and the only maps on which a `demo` ceiling and no ceiling at all give
/// DIFFERENT answers.
///
/// Only the five venues whose arm can reach live from a map: the four venues whose network is
/// their ceiling (a LIVE key set) and aster, which is switchless and picks its tier from WHICH key
/// set exists. Every other roster venue's arm hardcodes its demo endpoint, so there is no
/// live-tier map to write for it.
fn mainnet_arming_cases() -> &'static [(&'static str, &'static [(&'static str, &'static str)])] {
    &[
        ("binance", &[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")]),
        ("bybit", &[("BYBIT_LIVE_API_KEY", "k"), ("BYBIT_LIVE_API_SECRET", "s")]),
        (
            "okx",
            &[
                ("OKX_LIVE_API_KEY", "k"),
                ("OKX_LIVE_API_SECRET", "s"),
                ("OKX_LIVE_API_PASSPHRASE", "p"),
            ],
        ),
        ("hyperliquid", &[("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xkey")]),
        ("aster", &[("ASTER_LIVE_USER", "0xUser"), ("ASTER_LIVE_PRIVATE_KEY", "0xkey")]),
    ]
}

/// The boolean probe is the projection's own answer, not a parallel implementation — asserted
/// over the same matrix, because the pre-connect budget refusal rides on it and a divergence
/// there is a live-money defect rather than a cosmetic one.
#[test]
fn the_live_intent_probe_is_exactly_the_projection_above_paper() {
    use vike_config::VenueMode as Mode;
    let empty = HashMap::new();
    for (venue, kv) in live_arming_cases() {
        let armed = vars(kv);
        for map in [&armed, &empty] {
            for cap in Mode::ALL {
                assert_eq!(
                    vike_mount::would_mount_live_under(REGISTRY, venue, map, cap),
                    vike_mount::venue_arming_under(REGISTRY, venue, map, cap).0 != Mode::Paper,
                    "{venue} @ {cap}"
                );
            }
        }
    }
}

/// One `(venue, LIVE-tier keys, DEMO-tier keys)` row per switched venue (decision 0095: binance,
/// bybit, okx, hyperliquid) — deliberately its OWN table rather than a reuse of
/// [`live_arming_cases`], which for these four venues carries LIVE-tier keys only (its own doc
/// explains why: `would_mount_live` is an implicit `Live`-ceiling probe, so a DEMO-shaped row
/// there would probe false). Shared by the pure-projection test
/// ([`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`]) and its real-mount
/// twin ([`the_ceiling_alone_chooses_the_network_through_the_real_mount_too`]) so the two matrices
/// cannot silently drift apart, and so BOTH tiers are independently driven through the REAL mount
/// for every switched venue — not only the pure projection.
struct SwitchedVenueTiers {
    venue: &'static str,
    live_keys: &'static [(&'static str, &'static str)],
    demo_keys: &'static [(&'static str, &'static str)],
}

fn switched_venue_tier_cases() -> &'static [SwitchedVenueTiers] {
    &[
        SwitchedVenueTiers {
            venue: "binance",
            live_keys: &[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")],
            demo_keys: &[("BINANCE_DEMO_API_KEY", "k"), ("BINANCE_DEMO_API_SECRET", "s")],
        },
        SwitchedVenueTiers {
            venue: "bybit",
            live_keys: &[("BYBIT_LIVE_API_KEY", "k"), ("BYBIT_LIVE_API_SECRET", "s")],
            demo_keys: &[("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")],
        },
        SwitchedVenueTiers {
            venue: "okx",
            live_keys: &[
                ("OKX_LIVE_API_KEY", "k"),
                ("OKX_LIVE_API_SECRET", "s"),
                ("OKX_LIVE_API_PASSPHRASE", "p"),
            ],
            demo_keys: &[
                ("OKX_DEMO_API_KEY", "k"),
                ("OKX_DEMO_API_SECRET", "s"),
                ("OKX_DEMO_API_PASSPHRASE", "p"),
            ],
        },
        SwitchedVenueTiers {
            venue: "hyperliquid",
            live_keys: &[("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xkey")],
            demo_keys: &[("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xkey")],
        },
    ]
}

/// **Decision 0095: the ceiling alone chooses the network** for binance, bybit, okx and
/// hyperliquid, and the row says what is missing. No variable is consulted — the map below carries
/// none, and the process environment is not read on this path any more.
#[test]
fn the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing() {
    use vike_config::ArmingBlock as Block;
    use vike_config::VenueMode as Mode;

    for SwitchedVenueTiers { venue, live_keys, demo_keys } in switched_venue_tier_cases() {
        let venue = *venue;
        let (live, demo) = (vars(live_keys), vars(demo_keys));
        assert_eq!(
            vike_mount::venue_arming_under(REGISTRY, venue, &live, Mode::Live),
            (Mode::Live, Block::None),
            "{venue}"
        );
        assert_eq!(
            vike_mount::venue_arming_under(REGISTRY, venue, &demo, Mode::Live),
            (Mode::Paper, Block::LiveCredentialsAbsent),
            "{venue}: a `live` ceiling with only demo keys is PAPER — a mainnet host is never \
             signed with demo keys"
        );
        assert_eq!(
            vike_mount::venue_arming_under(REGISTRY, venue, &live, Mode::Demo),
            (Mode::Paper, Block::NoCredentials),
            "{venue}: a `demo` ceiling never reaches the LIVE key set"
        );
        assert_eq!(
            vike_mount::venue_arming_under(REGISTRY, venue, &demo, Mode::Demo),
            (Mode::Demo, Block::None),
            "{venue}"
        );
        let row = vike_config::VenueArming {
            venue: vike_config::roster_id(venue).expect("a roster venue"),
            label: AccountLabel::Default,
            ceiling: Mode::Live,
            effective: Mode::Paper,
            block: Block::LiveCredentialsAbsent,
        };
        assert!(row.why().contains("LIVE") && row.why().contains("paper"), "{}", row.why());
    }
    // deribit mounts through its own bridge and is testnet-only: a live ceiling still reaches demo.
    let deribit = vars(&[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")]);
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "deribit", &deribit, Mode::Live),
        (Mode::Demo, Block::DemoOnlyArm)
    );
}

/// **The REAL-MOUNT twin of [`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`].**
///
/// That test proves the pure projection (`venue_arming_under`) agrees that "a `live` ceiling with
/// only demo keys is PAPER" for the four switched venues — but proving the PROJECTION agrees with
/// itself is not the same evidence as proving the mount does. `live_arming_cases()` (the fixture
/// most tests in this suite draw from) deliberately carries LIVE-tier keys, not DEMO-tier, for
/// these four venues specifically so `would_mount_live`'s implicit `Live`-ceiling probe stays true
/// (decision 0095's own doc on that fn) — which means no map in that shared table can any longer
/// witness "DEMO-tier keys reach the REAL mount's paper path under a `live` ceiling" for
/// binance/bybit/okx/hyperliquid. This test drives [`mount`] — the real
/// `make_engine_with_legs` — directly with [`switched_venue_tier_cases`]'s own LIVE- and DEMO-tier
/// maps, all four (venue, key-tier, ceiling) combinations, so that property is witnessed by real
/// code and not only by the function under test agreeing with itself.
#[test]
fn the_ceiling_alone_chooses_the_network_through_the_real_mount_too() {
    use vike_config::VenueMode as Mode;

    for SwitchedVenueTiers { venue, live_keys, demo_keys } in switched_venue_tier_cases() {
        let venue = *venue;
        let (live, demo) = (vars(live_keys), vars(demo_keys));

        // LIVE keys, LIVE ceiling -> reaches the live path (the pre-connect budget refusal fires,
        // proving the mount classified this as live-intent rather than merely not-erroring).
        assert!(
            matches!(
                mount(venue, &live, Some(&ceiling(venue, Mode::Live))).0,
                Err(vike_mount::MountError::MissingRiskBudget { .. })
            ),
            "{venue}: LIVE-tier keys under a `live` ceiling must reach the live path"
        );

        // DEMO keys, LIVE ceiling -> PAPER. THE safety property this task exists to guard,
        // witnessed through the real mount rather than the pure projection alone: a `live`
        // ceiling never falls back to a venue's demo credentials.
        let (out, live_set) = mount(venue, &demo, Some(&ceiling(venue, Mode::Live)));
        out.unwrap_or_else(|e| {
            panic!(
                "{venue}: a `live` ceiling with only DEMO-tier keys must mount PAPER, not refuse \
                 to start: {e}"
            )
        });
        assert!(
            live_set.is_empty(),
            "{venue}: DEMO-tier keys armed a live exec client under a `live` ceiling"
        );

        // LIVE keys, DEMO ceiling -> PAPER (the real-mount twin of
        // `a_demo_ceiling_never_reaches_the_live_key_set`, over all four switched venues rather
        // than binance alone).
        let (out, live_set) = mount(venue, &live, Some(&ceiling(venue, Mode::Demo)));
        out.unwrap_or_else(|e| {
            panic!(
                "{venue}: a `demo` ceiling with only LIVE-tier keys must mount PAPER, not refuse \
                 to start: {e}"
            )
        });
        assert!(
            live_set.is_empty(),
            "{venue}: LIVE-tier keys armed an exec client under a `demo` ceiling"
        );

        // DEMO keys, DEMO ceiling -> reaches the demo path (still non-paper, still budget-gated —
        // the control that proves the two PAPER results above are the ceiling's doing and not a
        // fixture that never arms anything).
        assert!(
            matches!(
                mount(venue, &demo, Some(&ceiling(venue, Mode::Demo))).0,
                Err(vike_mount::MountError::MissingRiskBudget { .. })
            ),
            "{venue}: DEMO-tier keys under a `demo` ceiling must reach the demo path"
        );
    }
}

/// A venue this BUILD cannot mount reports [`vike_config::ArmingBlock::FeatureAbsent`] rather
/// than "no credentials" — the row renders the disagreement between a portable `policy.venues`
/// row and a binary's compiled features instead of hiding it behind a credential complaint.
///
/// The default build this module runs in (`mount_roster.rs`'s crate-level `cfg`) registers ibkr,
/// fxcm and polymarket `FeatureAbsent`, so these assertions hold wherever it runs; each venue's
/// feature-on half is in `crates/vike-tradehub/tests/ibkr_mount.rs`,
/// `crates/vike-tradehub/tests/fxcm_mount.rs` and `crates/vike-tradehub/tests/polymarket_mount.rs`.
#[test]
fn a_venue_this_build_cannot_mount_says_so_instead_of_blaming_credentials() {
    use vike_config::ArmingBlock as Block;
    use vike_config::VenueMode as Mode;
    let empty = HashMap::new();

    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "polymarket", &empty, Mode::Live),
        (Mode::Paper, Block::FeatureAbsent),
    );

    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "ibkr", &empty, Mode::Live),
        (Mode::Paper, Block::FeatureAbsent),
    );
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "fxcm", &empty, Mode::Live),
        (Mode::Paper, Block::FeatureAbsent),
    );

    // ⚠ dukascopy answered `NoLiveArm` here until 2026-09-09 — a fact this test kept apart from
    // "a feature is off" precisely because the two look identical to an operator. It has an arm
    // now, so with an EMPTY map the honest block is `NoCredentials`: the venue is mountable and
    // this box simply has no login for it. Keeping the old answer would have been the very
    // confusion the comment above guards against, one step down the ladder.
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "dukascopy", &empty, Mode::Live),
        (Mode::Paper, Block::NoCredentials),
    );
}

/// [`vike_mount::venue_arming`] is the projection over the WHOLE roster: one row per venue, each
/// carrying the ceiling it was asked about and the answer the per-venue call gives.
///
/// ⚠ **With an empty credential map and no `[accounts]` table there is exactly one row per
/// roster venue — the DEFAULT account's** — which is the table this projection returned before
/// it knew about accounts at all. That equality is the whole "a box with no accounts behaves
/// exactly as today" claim, asserted here at the projection rather than argued in prose.
#[test]
fn the_roster_projection_covers_every_venue_and_carries_its_ceiling() {
    use vike_config::VenueMode as Mode;
    let policy = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("bybit", Mode::Live).declare("binance", Mode::Demo),
        ..Default::default()
    };
    let rows = vike_mount::venue_arming(REGISTRY, &HashMap::new(), &policy);

    assert_eq!(rows.len(), vike_model::VENUES.len(), "one row per roster venue");
    let seen: Vec<&str> = rows.iter().map(|r| r.venue).collect();
    for venue in vike_model::VENUES {
        assert!(seen.contains(venue), "{venue} has no row");
    }
    for row in &rows {
        assert!(row.is_default_account(), "{}: an empty store names no second account", row.venue);
    }
    let by = |v: &str| rows.iter().find(|r| r.venue == v).expect("row").clone();
    assert_eq!(by("bybit").ceiling, Mode::Live);
    assert_eq!(by("binance").ceiling, Mode::Demo);
    assert_eq!(by("okx").ceiling, Mode::Paper, "an unnamed venue keeps the safe default");
    // …and every row agrees with the per-venue call it is built from.
    for row in &rows {
        assert_eq!(
            (row.effective, row.block),
            vike_mount::venue_arming_under(REGISTRY, row.venue, &HashMap::new(), row.ceiling),
        );
    }
}

/// **A LABELLED account in the credential store gets a row of its own** — and, with no
/// `[accounts]` line naming it, that row is PAPER with the block that says which line to write.
#[test]
fn a_labelled_account_in_the_store_gets_its_own_unarmed_row() {
    use vike_config::{ArmingBlock as Block, VenueMode as Mode};
    // `Demo`, not `Live`: `bybit_store_with_alt` plants DEMO-tier keys only, and decision 0095
    // means a `live` ceiling would no longer arm the default account from them (a mainnet host is
    // never signed with demo keys) — this test's subject is the LABELLED row, not the tier, so the
    // ceiling that actually matches the fixture's credentials is the one that keeps the default
    // row's "it IS armed" precondition true.
    let policy = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("bybit", Mode::Demo),
        ..Default::default()
    };
    let vars = bybit_store_with_alt();

    let rows = vike_mount::venue_account_arming(REGISTRY, "bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "the default account and ALT: {rows:?}");
    assert!(rows[0].is_default_account(), "the default account sorts FIRST");
    assert_ne!(rows[0].effective, Mode::Paper, "its credentials arm it, exactly as before");

    let alt = &rows[1];
    assert_eq!(alt.label.text(), Some("ALT"));
    assert_eq!(alt.effective, Mode::Paper, "a labelled account is not armed by the venue line");
    assert_eq!(alt.block, Block::AccountNotNamed);
    assert_eq!(alt.key(), "policy.accounts.bybit.ALT", "the row names the line to write");
    assert_eq!(alt.route_key(), "bybit#ALT");
    assert_eq!(rows[0].route_key(), "bybit", "the default account's routing does not move");
}

// ⚠ **THE HEADLINE — two armed accounts of one venue, and the SYMBOL reaching no arming
// decision — is asserted in `crates/vike-tradehub/tests/shared_book_report.rs`**, not here, and
// the reason is a gate rather than a preference: `crates/vike-ops/tests/settings_registry.rs`
// HARVESTS STRING LITERALS out of `src/` to find undeclared environment reads, and a fixture
// planting `HYPERLIQUID_DEMO_ACCOUNT_ADDRESS__ALT` here reads to that scanner as this library
// reading a variable nothing declares. A `tests/` file is outside its scan by construction, so
// a credential-shaped fixture belongs there — which is also where the rule's own unit tests
// live (`crates/vike-config/tests/venue_accounts_table.rs`).
//
// The test this replaced was `two_armed_accounts_on_one_symbol_refuse_the_labelled_one`, and it
// asserted the opposite of what is true: long BTC on the default account and short BTC on `ALT`
// is an ordinary spread, and the two accounts are two wallets holding two positions.
