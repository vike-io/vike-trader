//! `venues.json`: roster-complete, schema-complete, and the amend pair explained.

use vike_docs::{WIRING, venues_value, wiring_for};
use vike_model::VENUES;

use super::{ids, object_keys};

/// The per-record top-level keys, the `caps` keys, and the `tif` keys — the SCHEMA the module doc
/// promises. Asserted as exact SETS (not subsets) so a silently dropped OR silently added field is
/// loud here; a deliberate schema change edits this list beside `SCHEMA_VERSION`. Spelled sorted,
/// and [`object_keys`] sorts what it collects: key ORDER is deliberately outside the contract —
/// serde_json's map flavor flips with `preserve_order` (which the workspace's DataFusion crates
/// enable through feature unification whenever they share a build with vike-docs), so an order
/// assertion would pass standalone and fail in the roster lane, or vice versa.
const RECORD_KEYS: &[&str] = &[
    "amend_semantics",
    "amend_semantics_note",
    "attribution",
    "caps",
    "fees",
    "id",
    "margin",
    "tif",
    "wiring",
];
const CAPS_KEYS: &[&str] = &[
    "accepted_tifs",
    "backfill_bars",
    "backfill_ticks",
    "default_margin_mode",
    "live_data",
    "margin_modes",
    "max_batch",
    "supported_order_kinds",
    "supported_tifs",
    "supports_combo",
    "supports_modify",
    "supports_native_batch",
    "supports_post_only",
    "supports_reduce_only",
    "trigger_types",
];
const MARGIN_KEYS: &[&str] = &["isolated_wallet_adjustable", "offered_modes", "switch_mechanism"];
const TIF_KEYS: &[&str] = &["day", "fok", "gtc", "gtd", "ioc"];

/// The wiring map is roster-complete and roster-exact: one row per `vike_model::VENUES` entry, no
/// duplicate, no non-roster stray. This is the test the playbook demands of a per-venue table —
/// adding a venue to the roster reddens it until the row exists.
#[test]
fn wiring_map_is_roster_complete() {
    assert_eq!(WIRING.len(), VENUES.len(), "one WIRING row per roster venue, no strays");
    for &venue in VENUES {
        assert!(
            wiring_for(venue).is_some(),
            "roster venue {venue} has no WIRING row — add one in crates/vike-docs/src/venues.rs"
        );
    }
    let mut ids: Vec<&str> = WIRING.iter().map(|&(v, _)| v).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), WIRING.len(), "duplicate venue id in WIRING");
}

/// Every roster venue appears in `venues.json` exactly once, in roster order.
#[test]
fn every_roster_venue_appears_in_venues_json() {
    let venues = venues_value();
    assert_eq!(ids(&venues, "id"), VENUES.to_vec(), "one record per roster venue, in roster order");
}

/// Every record carries the full schema: the top-level keys, the caps/margin sub-objects, all five
/// TIF entries (each a tagged object), and non-empty tagged `fees`/`attribution` objects.
#[test]
fn every_record_has_all_fields() {
    let venues = venues_value();
    for record in venues.as_array().expect("array") {
        let id = record["id"].as_str().expect("id");
        assert_eq!(object_keys(record, id), RECORD_KEYS, "{id}: record keys");
        assert_eq!(object_keys(&record["caps"], id), CAPS_KEYS, "{id}: caps keys");
        assert_eq!(object_keys(&record["margin"], id), MARGIN_KEYS, "{id}: margin keys");
        assert_eq!(object_keys(&record["tif"], id), TIF_KEYS, "{id}: tif keys");
        for tif in TIF_KEYS {
            let entry = &record["tif"][tif];
            assert!(entry["outcome"].is_string(), "{id}: tif.{tif} has no outcome tag: {entry}");
        }
        for tagged in ["fees", "attribution"] {
            assert!(
                record[tagged]["kind"].is_string(),
                "{id}: {tagged} has no kind tag: {}",
                record[tagged]
            );
        }
        assert!(record["wiring"].is_string(), "{id}: wiring is a string");
        assert!(record["amend_semantics"].is_string(), "{id}: amend_semantics is a string");
    }
}

/// ⚠ **THE TWO AMEND FIELDS CANNOT CONTRADICT EACH OTHER UNEXPLAINED.** `caps.supports_modify`
/// (does an amend exist at all) and `amend_semantics` (what its quantity MEANS) are rendered from
/// two different tables, and this record is where they are flattened into one view — so this export
/// is where an unexplained pair becomes a reader's problem. It already has: a generated per-venue
/// page read `amend_semantics` FIRST and told the reader amending was supported on a venue whose
/// own exported caps row says it is not.
///
/// `vike_model::venues::venue_amend`'s `amend_semantics_agrees_with_venue_caps_supports_modify` gates the
/// same pairing over the TABLES. This is deliberately not that test moved: it gates the rendered
/// JSON, which is the artifact the pages read, and it gates the half the model test cannot see —
/// that the note is present exactly when it is needed. A model-side exception list with no rendered
/// counterpart is how the pair reached a page as a bare contradiction in the first place.
///
/// Both directions, so neither an unexplained contradiction nor a stale explanation can ship:
///
/// * COHERENT — `supports_modify: false` with `"unsupported"`, or `supports_modify: true` with
///   anything but `"unsupported"` — the note MUST be `null`. A note left behind after the tables
///   were reconciled ships an explanation for a contradiction that is no longer there.
/// * INCOHERENT — any other pair — the note MUST be a non-empty string. This is the assertion that
///   would have caught the finding: the pair is allowed to exist (ibkr's backend split is real),
///   but never bare.
#[test]
fn an_incoherent_amend_pair_is_explained_and_a_coherent_one_is_not() {
    let venues = venues_value();
    let records = venues.as_array().expect("array");
    assert_eq!(records.len(), VENUES.len(), "one record per roster venue");
    let mut explained = 0_usize;
    for record in records {
        let id = record["id"].as_str().expect("id");
        let modifiable = record["caps"]["supports_modify"]
            .as_bool()
            .unwrap_or_else(|| panic!("{id}: caps.supports_modify is a bool"));
        let sem = record["amend_semantics"].as_str().expect("amend_semantics");
        let note = &record["amend_semantics_note"];
        // "unsupported" is the ONLY amend_semantics value that means "no amend leaves the process",
        // so it is exactly the value that may sit beside supports_modify: false.
        let coherent = modifiable != (sem == "unsupported");
        if coherent {
            assert!(
                note.is_null(),
                "{id}: caps.supports_modify={modifiable} and amend_semantics={sem:?} agree, so \
                 amend_semantics_note must be null — delete the venue_amend CAPS_DISAGREEMENTS row \
                 it is rendered from; got {note}"
            );
        } else {
            let why = note.as_str().unwrap_or_else(|| {
                panic!(
                    "{id}: caps.supports_modify={modifiable} contradicts amend_semantics={sem:?} \
                     with NO amend_semantics_note. A consumer flattening these two fields cannot \
                     see the backend axis that makes the pair legitimate, and reads it as a bug. \
                     Either fix whichever value is wrong, or declare the disagreement in \
                     vike_model::venues::venue_amend's CAPS_DISAGREEMENTS with its reason."
                )
            });
            assert!(!why.trim().is_empty(), "{id}: amend_semantics_note is blank");
            explained += 1;
        }
    }
    // The exception list is a RATCHET in spirit: every explained pair is a venue whose two tables
    // disagree, and that set should shrink as evidence lands, never quietly grow. Spelled as an
    // upper bound naming today's one so a second one is a deliberate edit here.
    assert!(
        explained <= 1,
        "{explained} venues now render a contradictory amend pair (was 1: ibkr's socket/cpapi \
         backend split). Each new one needs its reason read, not just a CAPS_DISAGREEMENTS row."
    );
}
