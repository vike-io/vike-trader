use super::*;

/// **The ordering that makes this a ceiling.** Pinned as a full comparison chain rather than as
/// two `assert!(a < b)` lines, because the property stage 3 depends on is the TOTAL order.
#[test]
fn paper_is_below_demo_is_below_live() {
    assert!(VenueMode::Paper < VenueMode::Demo);
    assert!(VenueMode::Demo < VenueMode::Live);
    assert!(VenueMode::Paper < VenueMode::Live);
    // …and `ALL` is that order, so a menu and a comparison cannot disagree.
    let mut sorted = VenueMode::ALL;
    sorted.sort_unstable();
    assert_eq!(sorted, VenueMode::ALL);
    assert_eq!(VenueMode::default(), VenueMode::Paper, "the safe end is the default");
}

/// **THE property: the ceiling can only ever REFUSE.** Over every ordered pair of modes, the
/// capped result is never above EITHER input — so no `policy.toml` line can arm something the
/// existing mechanisms did not already arm.
///
/// Exhaustive over the 9 pairs rather than sampled: the whole set is three by three, and a
/// sampled version of a total-order property is a test that can be right by luck.
#[test]
fn a_ceiling_never_promotes_whatever_the_other_side_decided() {
    for ceiling in VenueMode::ALL {
        for decided in VenueMode::ALL {
            let effective = ceiling.cap(decided);
            assert!(effective <= ceiling, "{ceiling} capped UP to {effective}");
            assert!(effective <= decided, "{decided} was PROMOTED to {effective}");
            assert_eq!(effective, ceiling.min(decided), "cap must be min, never max");
        }
    }
    // The two cases the module doc names, spelled out so a reader sees the asymmetry.
    assert_eq!(VenueMode::Live.cap(VenueMode::Demo), VenueMode::Demo);
    assert_eq!(VenueMode::Paper.cap(VenueMode::Live), VenueMode::Paper);
    // …and `cap` is symmetric, which is what "the lower of the two" means.
    for a in VenueMode::ALL {
        for b in VenueMode::ALL {
            assert_eq!(a.cap(b), b.cap(a));
        }
    }
}

/// The file spelling round-trips through serde, and `as_str` is the same set the parser takes.
#[test]
fn the_three_spellings_round_trip() {
    for mode in VenueMode::ALL {
        let text = format!("m = {:?}\n", mode.as_str());
        #[derive(Deserialize)]
        struct One {
            m: VenueMode,
        }
        assert_eq!(toml::from_str::<One>(&text).expect("parses").m, mode);
        assert!(legal_modes().contains(mode.as_str()), "{mode} missing from {}", legal_modes());
    }
}

/// The default covers the WHOLE roster and nothing else — the property `provenance` derives its
/// rows from, and the one an empty map would silently break.
#[test]
fn the_default_is_one_paper_entry_per_roster_venue() {
    let p = VenuePolicy::default();
    assert_eq!(p.len(), VENUES.len());
    assert!(!p.is_empty());
    for venue in VENUES {
        assert_eq!(p.get(venue), VenueMode::Paper, "{venue}");
    }
    let held: Vec<&str> = p.iter().map(|(v, _)| v).collect();
    let mut roster = VENUES.to_vec();
    roster.sort_unstable();
    assert_eq!(held, roster, "exactly the roster, in id order");
}

/// An unknown venue string answers PAPER rather than panicking or being absent — the safe
/// answer for a caller that has a venue label from somewhere else.
#[test]
fn an_unknown_venue_reads_as_paper() {
    let p = VenuePolicy::default();
    assert_eq!(p.get("not-a-venue"), VenueMode::Paper);
    assert_eq!(p.get(""), VenueMode::Paper);
    assert_eq!(p.get("BYBIT"), VenueMode::Paper, "ids are lowercase; a shout is not one");
}

/// [`roster_id`] is exact and case-SENSITIVE; [`did_you_mean`] is a SEPARATE function that only
/// ever feeds a message. Keeping them apart is the property: a lookup that quietly accepted
/// either of the near misses below would be repairing the operator's file for them.
#[test]
fn the_roster_lookup_is_exact_and_the_hint_is_separate() {
    assert_eq!(roster_id("bybit"), Some("bybit"));
    assert_eq!(roster_id("Bybit"), None, "ids are lowercase and the lookup does not repair");
    assert_eq!(roster_id("bybitt"), None);

    assert_eq!(did_you_mean("BYBIT"), Some("bybit"), "a shouted id");
    assert_eq!(did_you_mean("bybitt"), Some("bybit"), "a slipped key");
    assert_eq!(did_you_mean("binanc"), Some("binance"), "a truncated one");
    // …and a name that is simply a different venue gets NO guess: naming a roster venue for
    // `kraken` would suggest trading somewhere vike has no bridge for.
    assert_eq!(did_you_mean("kraken"), None);
    assert_eq!(did_you_mean(""), None);
    assert_eq!(did_you_mean("ib"), None, "too short to be evidence of anything");
}

/// The map serializes as a flat `{venue: mode}` object — `#[serde(transparent)]`, so the
/// newtype is invisible on the wire and `policy.venues.<venue>` is a real leaf path.
///
/// ⚠ …and the DECLARED flag is invisible with it. It is `#[serde(skip)]`, so a set flag adds no
/// key: were it serialized it would appear as a fifteenth leaf beside the fourteen venues, and
/// `crates/vike-config/tests/provenance.rs`'s completeness gate would demand a
/// `policy.venues.declared` row for a fact no operator can set.
#[test]
fn the_map_serializes_transparently_as_venue_to_mode() {
    let mut p = VenuePolicy::default();
    p.set("bybit", VenueMode::Live);
    let table = toml::Table::try_from(&p).expect("serializes as a table");
    assert_eq!(table["bybit"].as_str(), Some("live"));
    assert_eq!(table["binance"].as_str(), Some("paper"));
    assert_eq!(table.len(), VENUES.len(), "the roster, and not one key more");
    assert!(p.is_declared(), "…while the flag itself is set and simply does not serialize");
}

/// **THE fact the ceiling map structurally cannot carry**: "everything at paper" and "nobody
/// ever wrote the table" produce the SAME map, and the stage-3 migration warning has to fire
/// for exactly one of them.
///
/// Asserted as a three-way comparison rather than as two `assert!`s, because the property is
/// that the two states are indistinguishable BY CEILING and distinguishable BY DECLARATION — a
/// test that only checked the flag would still pass if the maps had silently diverged, and the
/// whole point is that they do not.
#[test]
fn an_all_paper_table_is_the_default_map_and_is_still_declared() {
    let never_written = VenuePolicy::default();
    let mut written_all_paper = VenuePolicy::default();
    for venue in VENUES {
        written_all_paper.set(venue, VenueMode::Paper);
    }
    assert!(
        written_all_paper.iter().eq(never_written.iter()),
        "an all-paper table must be the SAME ceiling as no table — otherwise this fact is not \
             the one being distinguished"
    );
    assert!(!never_written.is_declared(), "the compiled-in default is nobody's decision");
    assert!(written_all_paper.is_declared(), "…and an all-paper table IS one");
}

/// [`VenuePolicy::declare`] is the out-of-crate builder: it states what it names, leaves every
/// other venue alone, marks the map declared, and IGNORES a non-roster id rather than panicking
/// (that map answers `paper` for the id either way — the refusal belongs on the FILE path,
/// where an operator believes they capped something).
#[test]
fn declare_states_one_venue_and_ignores_a_non_venue() {
    let p = VenuePolicy::default().declare("bybit", VenueMode::Live);
    assert_eq!(p.get("bybit"), VenueMode::Live);
    assert_eq!(p.get("binance"), VenueMode::Paper, "an unnamed venue keeps the default");
    assert_eq!(p.len(), VENUES.len(), "…and the map is still exactly the roster");
    assert!(p.is_declared());

    let ignored = VenuePolicy::default().declare("Bybit", VenueMode::Live);
    assert_eq!(ignored.get("bybit"), VenueMode::Paper, "a shouted id states nothing");
    assert_eq!(ignored.get("Bybit"), VenueMode::Paper, "…and is not stored under its own key");
    assert_eq!(ignored.len(), VENUES.len(), "a non-venue never joins the map");
}
