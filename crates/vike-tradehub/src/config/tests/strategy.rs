//! The `[strategy]` table and the two spellings of the A-S maker.

use super::*;

// ---------------------------------------------------------------------------------------------
// The `[strategy]` table.
// ---------------------------------------------------------------------------------------------

/// The headline: a strategy OTHER than the A-S maker resolves and lowers to a mountable box.
#[test]
fn a_registered_strategy_resolves_to_a_mountable_box() {
    let p = strategy_profile("grid").expect("a grid profile parses");
    let cfg = p.to_mount_config();
    assert!(p.resolve_strategy(&cfg).is_ok(), "grid must resolve into the mount box");
    // ...and so does every other name the registry says is live-capable, from ONE table.
    for (name, verdict) in vike_strategy::LIVE_CAPABLE {
        if verdict.blocker().is_some() {
            continue;
        }
        let p = strategy_profile(name).unwrap_or_else(|e| panic!("{name} profile: {e}"));
        assert!(p.resolve_strategy(&p.to_mount_config()).is_ok(), "{name} must resolve");
    }
}

/// The A-S maker is now ONE registered strategy rather than the hardcoded one — reachable by
/// name, exactly like the others.
#[test]
fn the_as_maker_is_reachable_by_name() {
    let p = strategy_profile("spread_maker").expect("parses");
    assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
}

// ---------------------------------------------------------------------------------------------
// The two spellings of the A-S maker (round-2 review, blocker 1).
//
// `[strategy] name = "spread_maker"` used to resolve through the REGISTRY arm
// (`SpreadMaker::from_params`), which reads `[strategy.params]` and NOTHING else — so on this
// daemon it mounted `qty = 1`, `tick_size = 0` and the `[0,1]` wall clamp instead of the
// venue-selected `AsParams`, while the very same profile with NO `[strategy]` table mounted the
// configured maker. MEASURED on a hyperliquid profile: registry `qty=1 tick=0` /
// `PriceDomain::UnitInterval` vs default `qty=0.005 tick=1.0` / `PriceDomain::Unbounded` — the
// configuration that posts ZERO orders on a $64k asset, under a startup log saying
// `strategy = spread_maker`.
// ---------------------------------------------------------------------------------------------

/// The headline property: naming the maker and not naming it are ONE construction, so they
/// cannot produce different makers. Compared on `SpreadMakerParams` — the maker's whole
/// observable knob surface, including the A-S bag — not on a hand-picked field or two.
#[test]
fn the_two_spellings_of_the_as_maker_are_one_construction() {
    for venue_toml in [
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005\n",
        "venue = \"polymarket\"\ntoken_id = \"TOK\"\nqty = 20.0\nresolution_ts_ms = 1793491200000\n",
    ] {
        let default_path = DaemonProfile::from_toml_str(venue_toml).expect("parses");
        let named = DaemonProfile::from_toml_str(&format!(
            "{venue_toml}[strategy]\nname = \"spread_maker\"\n"
        ))
        .expect("parses");

        // ⚠ Through `resolve_mount`, the function `main` actually calls (via
        // `resolve_strategy`) — NOT through the helper. Asserting on the helper alone would be
        // circular: the defect was that the named spelling took the OTHER route.
        let maker_of =
            |p: &DaemonProfile| match p.resolve_mount(&p.to_mount_config()).expect("resolves") {
                MountedStrategy::AsMaker(m) => m.params(),
                MountedStrategy::Registered(_) | MountedStrategy::Script { .. } => panic!(
                    "the A-S maker came from the REGISTRY arm, which reads `[strategy.params]` \
                     alone — it would mount qty=1 / tick_size=0 / the [0,1] wall clamp instead of \
                     this profile's maker fields"
                ),
            };
        let a = maker_of(&default_path);
        let b = maker_of(&named);
        assert_eq!(
            a, b,
            "`[strategy] name = \"spread_maker\"` must mount the SAME maker as no [strategy] \
                 table at all, for {venue_toml:?}"
        );

        // ...and it is the PROFILE's maker, not a defaults-only one: the qty the profile states
        // is the qty that mounts. (The registry arm's `SpreadMaker::from_params` on an empty
        // params table yields `qty = 1.0`, which is what made this a live hazard.)
        let cfg = named.to_mount_config();
        assert_eq!(b.qty.to_bits(), cfg.qty.to_bits(), "the profile's qty is the mounted qty");
        assert_eq!(
            b.avellaneda_stoikov.expect("A-S is on").price_domain,
            cfg.as_params.price_domain,
            "the VENUE-selected price domain is the mounted one — the [0,1] wall clamp on a \
                 $-scale asset is what posted zero orders"
        );
    }
}

/// `gueant_maker` is the same maker with the GLFT closed form selected — the registry's own
/// definition of the alias, applied to the PROFILE's config rather than to a default one.
#[test]
fn the_gueant_alias_is_the_same_maker_with_the_glft_model() {
    let base = "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005\n";
    let plain = DaemonProfile::from_toml_str(base).expect("parses");
    let gueant =
        DaemonProfile::from_toml_str(&format!("{base}[strategy]\nname = \"gueant_maker\"\n"))
            .expect("parses");
    let a = plain.mounted_maker(&plain.to_mount_config()).expect("maker").params();
    let g = gueant.mounted_maker(&gueant.to_mount_config()).expect("maker").params();
    let (a_as, g_as) = (a.avellaneda_stoikov.expect("A-S"), g.avellaneda_stoikov.expect("A-S"));
    assert_eq!(g_as.spread_model, SpreadModel::Gueant, "the alias selects GLFT");
    assert_ne!(a_as.spread_model, g_as.spread_model, "…and that is the ONLY difference:");
    assert_eq!(g.qty.to_bits(), a.qty.to_bits(), "…the profile's own maker fields still reach it");
    assert_eq!(SpreadModel::Gueant, g_as.spread_model);
    assert_eq!(
        vike_model::AsParams { spread_model: a_as.spread_model, ..g_as },
        a_as,
        "gueant_maker differs from the default mount in the spread model and nothing else"
    );
}

/// A strategy the registry resolves normally is NOT diverted through the maker path — the
/// routing above must be exactly the two maker names, never a catch-all.
#[test]
fn a_non_maker_strategy_is_not_diverted_through_the_maker_path() {
    let p = strategy_profile("grid").expect("parses");
    assert!(
        matches!(
            p.resolve_mount(&p.to_mount_config()).expect("resolves"),
            MountedStrategy::Registered(_)
        ),
        "`grid` must resolve through the registry, not as the A-S maker"
    );
    assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
}
