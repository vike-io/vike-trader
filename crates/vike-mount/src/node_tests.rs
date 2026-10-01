use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, DeclaredGridSource, MountInputs, MountOutcome, MountRequest,
    PaperCause, Resolution, VenueDeclaration, VenueMount,
};

/// A PLANTED hyperliquid row for [`a_mount_naming_an_unarmed_account_is_refused_by_name`]. This
/// crate names no bridge and holds no registry (docs/decisions/0096, amended 2026-09-29), so the
/// refusal is driven over one planted contract row, the way
/// `crates/vike-mount/src/contract_tests.rs` plants its rows. It declares the one fact the refusal
/// reads — the venue addresses a NAMED account — and resolves paper on every store, which is what
/// the real row answers with no `__ALT` credentials: the refusal asks what ARMED, and a row that
/// arms nothing is exactly that case.
struct PlantedHyperliquid;

impl VenueMount for PlantedHyperliquid {
    fn venue(&self) -> &'static str {
        "hyperliquid"
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            takes_recon_trigger: false,
            grid_source: DeclaredGridSource::NoGrid,
            book_identity: BookIdentity::Undeterminable { why: "a planted row names no book" },
            clock: ClockDecl::NotWired {
                reason: "a planted row publishes no server time",
                unmeasured_risk: None,
            },
        }
    }
    fn resolve(&self, _inputs: &MountInputs<'_>) -> Resolution {
        Resolution::Paper(PaperCause::NoCredentials)
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        MountOutcome::paper()
    }
}

static PLANTED_HYPERLIQUID: PlantedHyperliquid = PlantedHyperliquid;
static PLANTED_REGISTRY: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&PLANTED_HYPERLIQUID)];

/// **THE LOUD REFUSAL fires, and it fires on the arming table the fan-out selects with.**
///
/// [`super::refuse_unarmed_mount_accounts`] is the one refusal in this crate that does not
/// degrade, and a mutation replacing its whole body with `Ok(())` — an operator's `account =
/// "ALT"` accepted, no engine built for it, and `vike_core` then resolving the mount onto the
/// venue's DEFAULT account — was measured GREEN across `vike-run` and `vike-tradehub`. It was
/// cited in four doc comments and covered by nothing.
///
/// Three cases, because each is a different way to get the wrong answer:
///
/// * an account NO policy line names is refused, and the message names venue, account, symbol
///   and the two lines that would arm it — a refusal an operator cannot act on is a crash;
/// * an account named but resolving PAPER (no `__ALT` credentials behind the ceiling) is
///   refused too. This is the case that matters most: `venue_account_arming` answers with a
///   row for it, so a check written against "is there a row" rather than "did it ARM" would
///   pass it straight through to the default engine;
/// * an account-LESS mount is never refused, whatever the box arms. Every mount that has ever
///   shipped is that one.
#[test]
fn a_mount_naming_an_unarmed_account_is_refused_by_name() {
    let alt = vike_model::account_keys::AccountLabel::parse("ALT").expect("a legal label");
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
    let mount = |account: Option<vike_model::account_keys::AccountLabel>| {
        vec![super::MountAccount {
            venue: "hyperliquid".to_string(),
            symbol: "BTC".to_string(),
            account,
        }]
    };

    // (1) an account no policy line names.
    let bare = cfg(std::collections::HashMap::new(), crate::MountPolicy::default());
    let err = super::refuse_unarmed_mount_accounts(&bare, &mount(Some(alt.clone())))
        .expect_err("an unarmed account must REFUSE, never fall through to the default one");
    let said = err.to_string();
    for needle in ["hyperliquid", "ALT", "BTC", "policy.accounts"] {
        assert!(said.contains(needle), "the refusal must name `{needle}`: {said}");
    }

    // (2) …and one the policy DOES name, whose credentials are absent so it resolved paper.
    // `venue_account_arming` returns a row for it either way — the check is on what it ARMED.
    let named = cfg(
        std::collections::HashMap::new(),
        crate::MountPolicy {
            venues: vike_config::VenuePolicy::default()
                .declare("hyperliquid", vike_config::VenueMode::Demo)
                .declare_account("hyperliquid", &alt, vike_config::VenueMode::Demo),
            ..crate::MountPolicy::default()
        },
    );
    assert!(
        super::refuse_unarmed_mount_accounts(&named, &mount(Some(alt.clone()))).is_err(),
        "a ceiling that names an account arms nothing on its own — with no `__ALT` credentials \
             the account resolves PAPER, has no engine, and the mount must still be refused"
    );

    // (3) an account-LESS mount is never refused — the byte-identical path every shipped mount
    // takes, on a box that arms nothing at all.
    assert!(
        super::refuse_unarmed_mount_accounts(&bare, &mount(None)).is_ok(),
        "a mount naming no account resolves the venue's default engine as it always has"
    );
}

/// The one ordering rule the assembly owns: default engines are held by ascending
/// `engine_rank`, and a tie keeps MOUNT order. Planted rows; nothing is mounted.
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

/// THE BYTE-IDENTICAL PROPERTY at the derivation end: a leg-free mount contributes nothing to
/// ANY venue's engine, so `make_engine_with_legs` gets an empty list and touches nothing. This
/// is the state of every mount in the workspace today.
///
/// ⚠ NON-VACUOUS only because of the SECOND mount. With a lone leg-free mount the assertion
/// holds under any implementation that iterates legs at all — including one that ignores
/// `venue` and `wired_symbol` entirely — so it would discriminate nothing. Pairing it with a
/// mount that DOES declare legs is what gives it content: a derivation that leaked another
/// mount's legs onto a leg-free venue fails the first assertion, and the second pins that the
/// leak is not merely misfiled.
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

/// A SAME-VENUE leg (`MountLeg::same_venue`, no venue named) belongs to the MOUNT's venue and
/// nowhere else.
///
/// NON-VACUOUS: it asserts the OTHER venue gets nothing as well, so a derivation that ignored
/// the venue and handed every leg to every engine — which would silently grid a bybit engine
/// with a binance symbol's tick — fails on the second assertion rather than passing the first.
#[test]
fn a_same_venue_leg_reaches_only_its_own_mounts_venue() {
    let legs = vec![MountLeg::same_venue("ETHUSDT")];
    let mounts = || [("binance", legs.as_slice())].into_iter();
    assert_eq!(super::legs_for_venue(mounts(), "binance", "BTCUSDT"), vec!["ETHUSDT".to_string()]);
    assert!(super::legs_for_venue(mounts(), "bybit", "BTCUSDT").is_empty());
}

/// A CROSS-VENUE leg (`MountLeg::at`, the xEMM hedge shape) is gridded on the venue it will
/// actually be SENT to, not on the mount's own — the same resolution `resolve_intent_venue`
/// applies when routing that leg's orders.
///
/// NON-VACUOUS: the mount's own venue is asserted EMPTY, so a derivation that used
/// `mount.venue` unconditionally (the obvious wrong reading) fails — and it would fail in the
/// dangerous direction, gridding the maker engine for a symbol only the taker engine ever sees.
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

/// The venue's OWN wired symbol never becomes a leg: the engine's scalars already are that
/// symbol's grid, and a second row could only drift from them. This is also the live xEMM
/// mount's shape today — `XemmMountConfig::validate` refuses any hedge symbol the taker engine
/// is not already wired for, so its one declared leg lands here and is dropped.
#[test]
fn the_venues_own_wired_symbol_is_never_a_leg() {
    let legs = vec![MountLeg::at("BTC-USDT-SWAP", "okx")];
    let mounts = [("hyperliquid", legs.as_slice())];
    assert!(super::legs_for_venue(mounts.into_iter(), "okx", "BTC-USDT-SWAP").is_empty());
}

/// Two mounts declaring the SAME symbol on one venue produce ONE entry, in declaration order,
/// and a blank symbol produces none. On a venue whose grid lookup is a network call the
/// duplicate would be a wasted round trip; the ordering matters because `grid_by_symbol` is an
/// `IndexMap` whose insertion order reaches the serialized limits.
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
