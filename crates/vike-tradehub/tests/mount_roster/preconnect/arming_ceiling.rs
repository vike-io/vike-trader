//! THE ACCOUNT TIER (decision 0119): a `paper`, absent or inactive account never reaches a live client.

use std::collections::HashMap;

use vike_config::ArmingBlock;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::registry::REGISTRY;

use super::*;
use crate::account_book::{keyed_policy, with_keyed_account, with_keyed_row};

// ===========================================================================================
// THE ACCOUNT TIER (decision 0119: the `account` row's `tier` + `active` replaced the ceiling)
//
// ⚠ Every test below drives the REAL `make_engine_with_legs` with REAL live-arming credential
// shapes, and every one of them stays OFFLINE — because the tier holds. A regression makes
// this suite dial ten venues, which is the same stance (and the same wording) as the OKX
// passphrase and OANDA live-tier gates above.
//
// ⚠ The LIVE side is asserted through the pre-connect BUDGET REFUSAL rather than through
// `live_venues`, and that is not a weaker check dressed up — it is the only observable a
// network-free test HAS. Reaching `live_venues.insert` means the venue's arm ran, i.e. a
// blocking instrument fetch and a dialing exec thread; `MountError::MissingRiskBudget` fires
// one step earlier, from the arming probe, and firing it PROVES the mount classified the venue
// as live-intent at that tier. The paper side, where nothing dials, is asserted on
// `live_venues` directly.
//
// ⚠ A credential belongs to its ACCOUNT ROW (decision 0156 item 6), so "credentials present,
// account held at paper" is a row that HOLDS the fields and is itself paper or inactive — the
// fields are in the store, filed against the very account the table holds back.
// ===========================================================================================

/// **THE PROPERTY, over the WHOLE roster**: a venue whose DEFAULT account has no active
/// non-paper row never gets a live exec client, whatever credentials the store holds — and the
/// very same credentials on an active row at the arming tier do reach the live path, so the
/// refusal is the account table and not a broken fixture.
///
/// Four paper shapes, each a different `account_tier` answer: no policy at all, an UNREAD
/// directory (`MountPolicy::default()`), an active `paper`-tier row holding the fields, and an
/// INACTIVE row at the very tier these fields arm, holding them. (The first two carry no
/// directory to file a credential in: a process that read no store holds none.)
///
/// ⚠ The `None`-policy leg is not redundant with the `MountPolicy::default()` one, and it is
/// the leg that catches the fail-safe default being weakened: `policy.map_or(Paper, …)` and
/// `policy.map_or(Live, …)` agree on every `Some`, and differ only here.
#[test]
fn a_paper_moded_venue_never_gets_a_live_exec_client() {
    for (venue, tier, kv) in live_arming_cases() {
        // ANTI-VACUITY: these fields really would arm this venue at this tier. Without this line
        // every assertion below could pass against an account holding nothing.
        assert!(
            arms(venue, *tier, kv),
            "{venue}: the fixture must be a field set that ARMS, or this test proves nothing"
        );

        let unread = vike_mount::MountPolicy::default();
        let paper_row = keyed_policy(venue, AccountTier::Paper, kv);
        let inactive = with_keyed_row(
            vike_mount::MountPolicy::default(),
            account_row(venue, *tier, false),
            kv,
        );
        for (shape, policy, block) in [
            ("no policy", None, ArmingBlock::NoAccountRow),
            ("unread directory", Some(&unread), ArmingBlock::NoAccountRow),
            ("inactive row", Some(&inactive), ArmingBlock::AccountInactive),
        ] {
            let (out, live) = mount(venue, policy);
            let stopped = not_running(venue, out);
            assert_eq!(stopped.block, block, "{venue} ({shape}): the account does not run");
            assert!(live.is_empty(), "{venue} ({shape}): a live exec client was armed");
        }
        // …and the `paper`-tier row HOLDING the fields runs the simulator, which is no live client.
        let (out, live) = mount(venue, Some(&paper_row));
        let (engine, recon) = out.unwrap_or_else(|e| {
            panic!("{venue} (paper-tier row): a paper account runs the simulator: {e}")
        });
        assert!(live.is_empty(), "{venue} (paper-tier row): a live exec client was armed");
        assert!(recon.is_none(), "{venue}: a paper account never gets a reconcile handle");
        assert_eq!(engine.tier, AccountTier::Paper, "{venue}: at the tier its row states");
        assert_eq!(
            engine.fee_schedule,
            Some(vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, "BTCUSDT"))),
            "{venue} (paper-tier row): the paper mount is still tagged with the venue's fee schedule"
        );
        assert!(
            !probe(venue, AccountTier::Paper, kv),
            "{venue}: a paper-tier account has no live arm to probe"
        );

        // …and the CONTROL, which is what makes the refusals above the ACCOUNT TABLE's doing: the
        // same fields on an active row at their tier reach the live path. Observed as the
        // pre-connect budget refusal — no venue session is established, so this stays offline.
        let permitted = keyed_policy(venue, *tier, kv);
        match mount(venue, Some(&permitted)).0 {
            Err(vike_mount::MountError::MissingRiskBudget { venue: refused, .. }) => {
                assert_eq!(refused.as_str(), *venue, "the refusal must name the venue")
            }
            Err(other) => panic!(
                "{venue}: an active `{tier}` row holding these credentials did not reach the live \
                 path: {other}"
            ),
            Ok(_) => panic!(
                "{venue}: an active `{tier}` row holding these credentials did NOT reach the live \
                 path — the pre-connect budget refusal never fired, so the account table is \
                 refusing everything and the paper assertions above prove nothing"
            ),
        }

        // EXACTLY that venue: the same armed row leaves every OTHER venue on paper — each of them
        // holding its own arming fields on a row of its own that is NOT active — so a row arms its
        // own account rather than flipping a global switch.
        for (other, other_tier, other_kv) in live_arming_cases() {
            if other == venue {
                continue;
            }
            let beside = with_keyed_row(
                permitted.clone(),
                vike_secrets::Account { id: 2, ..account_row(other, *other_tier, false) },
                other_kv,
            );
            let (out, live) = mount(other, Some(&beside));
            let stopped = not_running(other, out);
            assert_eq!(
                stopped.block,
                ArmingBlock::AccountInactive,
                "{other} has no ACTIVE row of its own"
            );
            assert!(live.is_empty(), "{venue}'s row must not arm {other}");
        }
    }
}

/// **A `demo` account NEVER REACHES A `live` ACCOUNT'S KEY SET.** Decision 0095: an account's
/// tier picks binance's network, and its credentials are its own (decision 0156 item 6) — so with
/// the `live` account switched OFF and a `demo` account of the same venue active, the live keys
/// stay with the live row: the demo account holds none of its own and arms nothing, never a
/// mainnet host signed with the key set a switched-off account holds.
#[test]
fn a_demo_tier_never_reaches_the_live_key_set() {
    let keys: &[(&str, &str)] = &[("API_KEY", "k"), ("API_SECRET", "s")];
    // CONTROL FIRST, so the refusal below cannot be a fixture that never armed anything: for an
    // active `live` account holding them, these fields reach the live path (the pre-connect
    // budget refusal fires).
    assert!(probe("binance", AccountTier::Live, keys));
    assert!(matches!(
        mount("binance", Some(&keyed_policy("binance", AccountTier::Live, keys))).0,
        Err(vike_mount::MountError::MissingRiskBudget { .. })
    ));

    // …and with that `live` row INACTIVE (still holding them) beside an active `demo` row holding
    // nothing, the account trades `demo` and arms nothing at all.
    let switched = with_keyed_account(
        with_keyed_row(
            vike_mount::MountPolicy::default(),
            account_row("binance", AccountTier::Live, false),
            keys,
        ),
        "binance",
        &AccountLabel::Default,
        AccountTier::Demo,
        &[],
    );
    let rows =
        vike_mount::venue_account_arming(REGISTRY, "binance", &HashMap::new(), Some(&switched));
    assert_eq!(
        (rows[0].tier, rows[0].effective, rows[0].block),
        (Some(AccountTier::Demo), None, vike_config::ArmingBlock::NoCredentials),
        "a `demo` account resolves its OWN row's credentials, and the live row's are not its own"
    );
    let (out, live) = mount("binance", Some(&switched));
    let stopped = not_running("binance", out);
    assert_eq!(stopped.tier, Some(AccountTier::Demo), "a demo account holding none of its own");
    assert_eq!(stopped.block, ArmingBlock::NoCredentials);
    assert!(live.is_empty(), "the LIVE account's key set armed a `demo` account");
}

/// **THE MEASURED HOLE, closed by the account model.** Aster has never had a `{VENUE}_MAINNET`
/// switch, and its arm once tried the LIVE key set FIRST — so a live pair in the credential store
/// was by itself an authenticated MAINNET session on a daemon that never asked for one (observed
/// on the CI box, inside a nine-venue live set).
///
/// Aster's network is now its account's TIER (like binance's), and a key set is the account's own:
/// the pair reaches mainnet only through an ACTIVE `live` row holding it. Below `live` — the `live`
/// row switched off, a `demo` or `paper` row active and holding nothing — nothing arms; and a
/// `live` account with no key set of its own is PAPER (`LiveCredentialsAbsent`), never the testnet
/// session of the `demo` account beside it (no testnet fallback, decision 0119).
#[test]
fn aster_reaches_mainnet_only_through_an_active_live_account_holding_its_pair() {
    let pair: &[(&str, &str)] = &[("USER", "0xUser"), ("PRIVATE_KEY", "0xkey")];
    // CONTROL: the pair genuinely arms this venue under an active `live` row — mainnet.
    assert!(probe("aster", AccountTier::Live, pair));
    assert!(matches!(
        mount("aster", Some(&keyed_policy("aster", AccountTier::Live, pair))).0,
        Err(vike_mount::MountError::MissingRiskBudget { .. })
    ));

    for capped in [AccountTier::Demo, AccountTier::Paper] {
        let switched = with_keyed_account(
            with_keyed_row(
                vike_mount::MountPolicy::default(),
                account_row("aster", AccountTier::Live, false),
                pair,
            ),
            "aster",
            &AccountLabel::Default,
            capped,
            &[],
        );
        assert!(
            !vike_mount::would_mount_live_under_policy(
                REGISTRY,
                "aster",
                &HashMap::new(),
                Some(&switched)
            ),
            "{capped}: the switched-off `live` account's pair must arm nothing — the account's \
             tier is the only refusal that exists"
        );
        let (out, live) = mount("aster", Some(&switched));
        match capped {
            AccountTier::Paper => {
                let (engine, _) =
                    out.unwrap_or_else(|e| panic!("a paper aster account runs the simulator: {e}"));
                assert_eq!(engine.tier, AccountTier::Paper);
            }
            _ => {
                let stopped = not_running("aster", out);
                assert_eq!(stopped.tier, Some(capped), "{capped}: the account does not run");
            }
        }
        assert!(
            live.is_empty(),
            "{capped}: the `live` account's pair armed a real mainnet account"
        );
    }

    // …and the SAME pair still arms a `demo` account holding it, so what the tier refused is the
    // other account's key set and not the venue.
    assert!(probe("aster", AccountTier::Demo, pair));

    // …and the NO-DOWNGRADE half (the owner's ruling on decision 0119): a `live` account holding
    // NO pair of its own is PAPER, never the testnet session a `demo` account beside it holds.
    assert_eq!(
        projected("aster", AccountTier::Live, &[]),
        (None, vike_config::ArmingBlock::LiveCredentialsAbsent),
        "a `live` aster account with no key set must project PAPER, never demo"
    );
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let beside = with_keyed_account(
        keyed_policy("aster", AccountTier::Live, &[]),
        "aster",
        &alt,
        AccountTier::Demo,
        pair,
    );
    let (out, live) = mount("aster", Some(&beside));
    let stopped = not_running("aster", out);
    assert_eq!(stopped.block, ArmingBlock::LiveCredentialsAbsent);
    assert!(live.is_empty(), "a `live` aster account reached a `demo` account's testnet pair");
}

/// **The regression the D1 fix exists for, on the account's own figure.** Until 2026-09-27
/// `paper_engine` took no `account` and so could not read a per-account exposure figure. This plants `account.max_exposure` on the DEFAULT account's `paper` row and asserts
/// the paper engine applies it, mirroring the live twin's own fold in
/// `make_engine_for_account`'s post-match tail.
///
/// The second half pins the fold's DIRECTION on the paper side: the row's figure is a `min` with
/// the box-wide `policy.max_account_exposure`, so it narrows a looser box figure and never
/// widens a tighter one.
#[test]
fn a_paper_engine_applies_the_per_account_exposure_ceiling() {
    let cap = 42_000.0;
    let with_cap = |venue: &str, tier: AccountTier, box_wide: Option<f64>, kv: &[(&str, &str)]| {
        let row = vike_secrets::Account {
            max_exposure: vike_secrets::MaxExposure::new(cap),
            ..account_row(venue, tier, true)
        };
        with_keyed_row(
            vike_mount::MountPolicy {
                max_account_exposure: box_wide,
                ..vike_mount::MountPolicy::default()
            },
            row,
            kv,
        )
    };
    for (venue, _tier, kv) in live_arming_cases() {
        let capped = mount(venue, Some(&with_cap(venue, AccountTier::Paper, None, kv)))
            .0
            .unwrap_or_else(|e| panic!("{venue} paper-tier: {e}"))
            .0;
        assert_eq!(
            capped.gate.limits.max_account_exposure,
            Some(cap),
            "{venue}: a paper-tier engine must apply the DEFAULT account's `max_exposure`, \
                 exactly as the live twin's post-match tail does"
        );

        for (box_wide, expected) in [(cap * 2.0, cap), (cap / 2.0, cap / 2.0)] {
            let engine =
                mount(venue, Some(&with_cap(venue, AccountTier::Paper, Some(box_wide), kv)))
                    .0
                    .unwrap_or_else(|e| panic!("{venue} paper-tier, box-wide {box_wide}: {e}"))
                    .0;
            assert_eq!(
                engine.gate.limits.max_account_exposure,
                Some(expected),
                "{venue}: box-wide {box_wide} and the row's {cap} must fold to the TIGHTER one"
            );
        }
    }
}
