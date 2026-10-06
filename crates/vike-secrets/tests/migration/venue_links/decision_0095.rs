//! Decision 0095 on a row whose two spellings disagree.

use super::*;

// ---------------------------------------------------------------------------------------------
// Decision 0095 on a row whose two spellings disagree
// ---------------------------------------------------------------------------------------------

/// `(the venue its NUMBER names, its text, its mode)` for every `venue_arming` row, by that name.
fn arming_by_number(fx: &Fixture) -> Vec<(String, String, String)> {
    let conn = fx.conn();
    let mut stmt = conn
        .prepare(
            "SELECT v.name, a.venue, a.mode FROM venue_arming a \
             JOIN venue v ON v.id = a.venue_id ORDER BY v.name",
        )
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .map(Result::unwrap)
        .collect()
}

/// A row whose number and text disagree is rewritten from `live` to `demo` when EITHER names a
/// venue decision 0095 switches: this binary reads the number, a ROLLBACK reads the text, and `demo`
/// is the safe answer to both. Two rows, mirror images: one numbered aster (unswitched) and spelled
/// binance, one numbered binance and spelled aster.
#[test]
fn decision_0095_rewrites_a_live_row_when_either_its_number_or_its_text_names_a_switched_venue() {
    let fx = planted();
    let live = |venue: &str| ArmingRow {
        venue: venue.to_string(),
        label: None,
        mode: "live".to_string(),
        max_exposure: None,
    };
    vike_secrets::write_settings_in(
        fx.dir(),
        &StoredSettings { arming: vec![live("aster"), live("binance")], ..Default::default() },
    )
    .expect("two `live` ceilings");
    {
        let conn = fx.conn();
        conn.execute(
            "UPDATE venue_arming SET venue = \
             CASE venue WHEN 'aster' THEN 'binance' WHEN 'binance' THEN 'aster' END",
            [],
        )
        .expect("swap the two rows' text cells");
    }
    vike_secrets::live_means_mainnet::unmark_live_means_mainnet(fx.dir());
    assert_eq!(
        arming_by_number(&fx),
        [
            ("aster".to_string(), "binance".to_string(), "live".to_string()),
            ("binance".to_string(), "aster".to_string(), "live".to_string()),
        ],
        "premise: both rows are `live`, and each one's text names the other venue"
    );

    let vike_secrets::live_means_mainnet::LiveMeansMainnet::Applied { rewrites, .. } =
        apply_0095(&fx)
    else {
        panic!("a pending store is migrated");
    };
    assert_eq!(rewrites.len(), 2, "both rows are rewritten: {rewrites:?}");
    // ⚠ …and each is REPORTED under the key this binary reads it by, its number's venue: the row
    // numbered aster was this binary's `policy.venues.aster`, and that is the ceiling that moved.
    // Until review the row was reported under its TEXT's switched venue, so both rewrites said
    // `policy.venues.binance` and the aster ceiling that changed was named nowhere.
    assert_eq!(
        rewrites.iter().map(|r| r.key()).collect::<Vec<_>>(),
        ["policy.venues.aster", "policy.venues.binance"],
        "each rewrite names the key this binary reads the row by"
    );
    assert_eq!(
        arming_by_number(&fx).into_iter().map(|(_, _, mode)| mode).collect::<Vec<_>>(),
        ["demo", "demo"],
        "…on the store, not only in the report"
    );
    assert!(marked_0095(&fx), "…and the marker is written");
}
