//! `node`'s unit tests: account refusal by name, engine rank order, and which legs reach a mount.

use vike_bridge_core::venue_mount::{PaperCause, Resolution, VenueDeclaration};
use vike_bridge_core::venue_mount_fixture::{PLANTED_DECLARATION, PlantedMount};

/// A PLANTED hyperliquid row for [`a_mount_naming_an_unarmed_account_is_refused_by_name`] (this
/// crate holds no registry, docs/decisions/0096; planted as in
/// `crates/vike-mount/src/contract_tests/mod.rs`). It declares the one fact the refusal reads —
/// the venue addresses a NAMED account — and resolves paper on every store, as the real row does
/// with no `__ALT` credentials: the refusal asks what ARMED.
static PLANTED_HYPERLIQUID: PlantedMount = PlantedMount {
    declaration: VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION },
    ..PlantedMount::new("hyperliquid", Resolution::Paper(PaperCause::NoCredentials))
};
static PLANTED_REGISTRY: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&PLANTED_HYPERLIQUID)];

/// **THE LOUD REFUSAL fires, and it fires on the arming table the fan-out selects with.**
///
/// [`super::refuse_unarmed_mount_accounts`] is this crate's one refusal that does not degrade. A
/// mutation replacing its body with `Ok(())` (`account = "ALT"` accepted, no engine for it,
/// `vike_core` resolving the mount onto the DEFAULT account) was measured GREEN across
/// `vike-run` and `vike-tradehub`: cited in four doc comments, covered by nothing.
///
/// Three cases, each a different wrong answer:
///
/// * an account NO account row names is refused, naming venue, account, symbol and the commands
///   that would arm it (a refusal an operator cannot act on is a crash);
/// * an account named but resolving PAPER (no `__ALT` credentials) is refused too — the case
///   that matters most: `venue_account_arming` returns a row for it, so an "is there a row" check
///   would pass it to the default engine;
/// * an account-LESS mount (every shipped mount) is never refused, whatever the box arms.
#[test]
fn a_mount_naming_an_unarmed_account_is_refused_by_name() {
    let alt =
        vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let cfg = |vars: std::collections::HashMap<String, String>, policy: crate::MountPolicy| {
        super::NodeConfig {
            registry: &PLANTED_REGISTRY,
            markets: &[],
            vars,
            properties_rec: None,
            seed_cash: 10_000.0,
            recon_enabled: false,
            core_config: vike_core::CoreConfig::default(),
            risk_profile: None,
            policy,
        }
    };
    let mount = |account: Option<vike_model::accounts::account_keys::AccountLabel>| {
        vec![super::MountAccount {
            venue: "hyperliquid".to_string(),
            symbol: "BTC".to_string(),
            account,
        }]
    };

    // (1) an account no account row names.
    let bare = cfg(std::collections::HashMap::new(), crate::MountPolicy::default());
    let err = super::refuse_unarmed_mount_accounts(&bare, &mount(Some(alt.clone())))
        .expect_err("an unarmed account must REFUSE, never fall through to the default one");
    let said = err.to_string();
    for needle in ["hyperliquid", "ALT", "BTC", "vike-cli secrets account activate"] {
        assert!(said.contains(needle), "the refusal must name `{needle}`: {said}");
    }

    // (2) …and one an ACTIVE row DOES name, resolved paper (no credentials): the check is on ARMED.
    let default = vike_model::accounts::account_keys::AccountLabel::Default;
    let named = cfg(
        std::collections::HashMap::new(),
        crate::MountPolicy::default()
            .with_account("hyperliquid", &default, vike_config::VenueMode::Demo)
            .with_account("hyperliquid", &alt, vike_config::VenueMode::Demo),
    );
    assert!(
        super::refuse_unarmed_mount_accounts(&named, &mount(Some(alt.clone()))).is_err(),
        "an account row arms nothing on its own — with no `__ALT` credentials the account \
             resolves PAPER, has no engine, and the mount must still be refused"
    );

    // (3) an account-LESS mount is never refused, even on a box that arms nothing.
    assert!(
        super::refuse_unarmed_mount_accounts(&bare, &mount(None)).is_ok(),
        "a mount naming no account resolves the venue's default engine as it always has"
    );
}

/// A planted [`super::NodeConfig`] over [`PLANTED_REGISTRY`] with the given table; no
/// credentials, no mounts, an all-default policy.
#[cfg(debug_assertions)]
fn planted_node_cfg(markets: &'static [super::WiredMarket]) -> super::NodeConfig {
    super::NodeConfig {
        registry: &PLANTED_REGISTRY,
        markets,
        vars: std::collections::HashMap::new(),
        properties_rec: None,
        seed_cash: 10_000.0,
        recon_enabled: false,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: None,
        policy: crate::MountPolicy::default(),
    }
}

/// An empty preflight report: the tests below reach [`super::build_node_with_preflight`]'s
/// assembly without the real preflight.
#[cfg(debug_assertions)]
fn no_objection() -> crate::preflight::PreflightReport {
    crate::preflight::PreflightReport::default()
}

/// **Two rows naming ONE venue panic a debug build before anything is mounted** (the second
/// would mount the default account twice); release does not check (`build_node`'s `# Panics`).
/// The ranks differ, so only the venue half of the `debug_assert!` can fire.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "names a venue or an engine_rank twice")]
fn a_table_naming_a_venue_twice_panics_in_a_debug_build() {
    static TWICE: [super::WiredMarket; 2] = [
        super::WiredMarket {
            venue: "hyperliquid",
            symbol: "BTC",
            reconnect_poke: false,
            engine_rank: 0,
        },
        super::WiredMarket {
            venue: "hyperliquid",
            symbol: "ETH",
            reconnect_poke: false,
            engine_rank: 1,
        },
    ];
    let _ = super::build_node_with_preflight(planted_node_cfg(&TWICE), &no_objection());
}

/// **Two rows sharing an `engine_rank` panic a debug build too** (else the table decides their
/// order). The venues differ (the second is no roster id, so no registry row is consulted
/// first), so only the rank half can fire.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "names a venue or an engine_rank twice")]
fn a_table_sharing_an_engine_rank_panics_in_a_debug_build() {
    static SHARED_RANK: [super::WiredMarket; 2] = [
        super::WiredMarket {
            venue: "hyperliquid",
            symbol: "BTC",
            reconnect_poke: false,
            engine_rank: 0,
        },
        super::WiredMarket {
            venue: "planted-second-venue",
            symbol: "S",
            reconnect_poke: false,
            engine_rank: 0,
        },
    ];
    let _ = super::build_node_with_preflight(planted_node_cfg(&SHARED_RANK), &no_objection());
}

/// [`super::in_engine_order`]'s rule: default engines held by ascending `engine_rank`. A tie
/// keeps MOUNT order (stable sort), though a debug `build_node` panics on one first
/// ([`a_table_sharing_an_engine_rank_panics_in_a_debug_build`]).
#[test]
fn the_default_engines_are_held_in_rank_order() {
    let row = |venue, engine_rank| super::WiredMarket {
        venue,
        symbol: "S",
        reconnect_poke: false,
        engine_rank,
    };
    let mounted =
        vec![(row("a", 2), 'a'), (row("b", 0), 'b'), (row("c", 1), 'c'), (row("d", 1), 'd')];
    let got: Vec<char> = super::in_engine_order(mounted).into_iter().map(|(_, t)| t).collect();
    assert_eq!(got, ['b', 'c', 'd', 'a']);
}

// ---- the declared-leg derivation feeding `crate::make_engine_with_legs` ----

use vike_core::MountLeg;

/// THE BYTE-IDENTICAL PROPERTY at the derivation end: a leg-free mount (every mount today)
/// contributes nothing to ANY venue's engine, so `make_engine_with_legs` gets an empty list.
///
/// ⚠ NON-VACUOUS only because of the SECOND mount: alone, it holds under any implementation,
/// even one ignoring `venue` and `wired_symbol`. Beside a mount that DOES declare legs, a leak
/// onto a leg-free venue fails the first assertion; the second pins it is not merely misfiled.
#[test]
fn a_leg_free_mount_gets_nothing_even_beside_a_mount_that_declares_legs() {
    let none: Vec<MountLeg> = Vec::new();
    let some = vec![MountLeg::same_venue("ETHUSDT")];
    let mounts = || [("binance", none.as_slice()), ("bybit", some.as_slice())].into_iter();
    assert!(
        super::legs_for_venue(mounts(), "binance", "BTCUSDT").is_empty(),
        "binance declares no legs, so bybit's must not reach it"
    );
    assert_eq!(
        super::legs_for_venue(mounts(), "bybit", "SOLUSDT"),
        vec!["ETHUSDT".to_string()],
        "...while the mount that DOES declare one still gets it"
    );
}

/// A SAME-VENUE leg (`MountLeg::same_venue`) belongs to the MOUNT's venue only.
///
/// NON-VACUOUS: the OTHER venue is asserted empty, so handing every leg to every engine
/// (silently gridding a bybit engine with a binance symbol's tick) fails the second assertion.
#[test]
fn a_same_venue_leg_reaches_only_its_own_mounts_venue() {
    let legs = vec![MountLeg::same_venue("ETHUSDT")];
    let mounts = || [("binance", legs.as_slice())].into_iter();
    assert_eq!(super::legs_for_venue(mounts(), "binance", "BTCUSDT"), vec!["ETHUSDT".to_string()]);
    assert!(super::legs_for_venue(mounts(), "bybit", "BTCUSDT").is_empty());
}

/// A CROSS-VENUE leg (`MountLeg::at`, the xEMM hedge) is gridded on the venue it is SENT to —
/// the resolution `resolve_intent_venue` applies when routing its orders.
///
/// NON-VACUOUS: the mount's own venue is asserted EMPTY, so using `mount.venue` unconditionally
/// fails (the dangerous direction: gridding the maker for a symbol only the taker sees).
#[test]
fn a_cross_venue_leg_is_gridded_on_the_venue_it_routes_to() {
    let legs = vec![MountLeg::at("BTC-USDT-SWAP", "okx")];
    let mounts = || [("hyperliquid", legs.as_slice())].into_iter();
    assert_eq!(
        super::legs_for_venue(mounts(), "okx", "ETH-USDT-SWAP"),
        vec!["BTC-USDT-SWAP".to_string()]
    );
    assert!(super::legs_for_venue(mounts(), "hyperliquid", "BTC").is_empty());
}

/// The venue's OWN wired symbol never becomes a leg (the engine's scalars are its grid; a second
/// row could only drift). The live xEMM shape: `XemmMountConfig::validate` refuses a hedge
/// symbol the taker is not wired for, so its one leg lands here and is dropped.
#[test]
fn the_venues_own_wired_symbol_is_never_a_leg() {
    let legs = vec![MountLeg::at("BTC-USDT-SWAP", "okx")];
    let mounts = [("hyperliquid", legs.as_slice())];
    assert!(super::legs_for_venue(mounts.into_iter(), "okx", "BTC-USDT-SWAP").is_empty());
}

/// The SAME symbol from two mounts on one venue is ONE entry (a duplicate grid lookup can be a
/// wasted round trip), in declaration order (`grid_by_symbol` is an `IndexMap` whose order
/// reaches the serialized limits); a blank symbol is none.
#[test]
fn repeats_collapse_and_blanks_are_dropped_in_declaration_order() {
    let a = vec![MountLeg::same_venue("ETHUSDT"), MountLeg::same_venue("   ")];
    let b = vec![MountLeg::same_venue("SOLUSDT"), MountLeg::same_venue("ETHUSDT")];
    let mounts = [("binance", a.as_slice()), ("binance", b.as_slice())];
    assert_eq!(
        super::legs_for_venue(mounts.into_iter(), "binance", "BTCUSDT"),
        vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]
    );
}
