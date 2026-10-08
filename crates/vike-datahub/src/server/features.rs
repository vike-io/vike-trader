//! The `Welcome` advertisement (`served_features`) and the key-less delete refusal.

use vike_datahub_client::proto::{FEATURE_BACKFILL_CANCEL, FEATURE_HISTORY_CHANNELS};
use vike_datahub_client::{
    DATA_PLANE_SENTINEL, FEATURE_AUTH, FEATURE_BACKFILL, FEATURE_BACKFILL_FUNDING,
    FEATURE_COVERAGE, FEATURE_DELETE_SERIES, FEATURE_MARKET_DATA, FEATURE_SCAN_BOOK_UPDATES,
    FEATURE_SCAN_COHORT, FEATURE_SCAN_DEPTH, FEATURE_SCAN_EQUITY, FEATURE_SCAN_EXEC_FILLS,
    FEATURE_SCAN_LIMIT, FEATURE_SCAN_PERP_METRICS, FEATURE_SEED_CLASS, FEATURE_SEED_SERIES,
    FEATURE_SERIES_FACTS, FEATURE_VENUE_CATALOG, md_venue_feature,
};

use crate::import::ImportLane;
use crate::md::MdHub;

/// The message a KEY-LESS server answers `DeleteSeries` with. A `const` because two things must be
/// able to name it: this refusal, and the test that proves it is the same server refusing on the
/// path the client actually takes.
pub const KEYLESS_DELETE_REFUSAL: &str = "this datahub holds no node keys, so it serves no delete verb at all. A key-less server \
     authenticates nothing — its handshake informs, it does not gate — so `Scope::Write` would be \
     a word nothing enforces, and an irreversible verb behind a word is the state \
     docs/decisions/0025-datahub-remote-posture.md exists to prevent. Configure \
     VIKE_DATAHUB_OBSERVE_KEY / VIKE_DATAHUB_CONTROL_KEY on this server, or run the delete on the \
     box with `vike-cli data hist rm --store DIR`.";

/// The verbs THIS server answers, advertised in the [`Response::Welcome`] handshake. Kept in sync
/// with the arms of [`handle_request`]: the four `HistStore` read verbs the `RemoteHistStore` seam
/// consumes, and the four store-metadata verbs beside them
/// (`list_series`/`inventory`/`series_gaps`/[`FEATURE_COVERAGE`]).
///
/// ⚠ **The compute strings are GONE, and their absence is the negotiation** (ruling 7 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`). This list used to open
/// with `backtest` and carry `list_strategies`, `run_sweep_profile`, `run_walkforward_profile` and
/// — on a `serve-datafusion` build — `run_slice`/`run_sweep`/`run_walkforward`. Those seven verbs
/// are served by `vike-backend backtest --addr` now, and a well-behaved client that reads this list
/// learns so at the HANDSHAKE, before it ships a profile it would only be refused for. A client
/// that sends one anyway still gets the named refusal
/// (`vike_datahub_client::proto`'s `wrong_plane_message`) rather than a drop — the advertisement is
/// what stops the send, the refusal is what holds for a client that does not read it, and neither
/// substitutes for the other. This is the same three-legged shape [`FEATURE_BACKFILL`] and
/// [`FEATURE_DELETE_SERIES`] already use, applied to a REMOVAL rather than an addition.
///
/// ⚠ **`run_sweep_profile` and `run_sweep` are spelled the old way deliberately.** The
/// `sweep` -> `paramscan` rename moved the Rust identifiers and left every NEGOTIATED TOKEN where
/// it was — an older client compares a capability string literally, so renaming one refuses every
/// peer that shipped before the rename. The rename pass rewrote this paragraph anyway, which is how
/// a doc naming two strings that appear on no wire shipped once already; the compute daemon's
/// `served_features` (`crates/vike-backtest/src/compute_server.rs`) is what actually pushes them,
/// and it is the authority.
///
/// `has_backfill` is a RUNTIME fact, not a cfg: [`FEATURE_BACKFILL`] is advertised exactly when a
/// collector table is MOUNTED, because "compiled with `backfill-serve`" and "can actually serve a
/// backfill" are the same thing only when the bin wired a table in — a `serve()` entry inside a
/// `backfill-serve` test build still must not advertise what it will refuse. [`FEATURE_BACKFILL_CANCEL`]
/// rides the same fact: the registry it opens is the mounted table's. `has_backfill_funding`
/// is the same fact one level down: the mounted table carries a FUNDING lane
/// (`crate::backfill::BackfillTable::has_funding`). `import` is the same fact for the archive import
/// lane: `crate::import::advertised` pushes the capability and its format entries exactly when a
/// lane is mounted.
pub(super) fn served_features(
    has_backfill: bool,
    has_backfill_funding: bool,
    requires_auth: bool,
    md: Option<&MdHub>,
    has_seed: bool,
    has_catalog: bool,
    import: Option<&ImportLane>,
) -> Vec<String> {
    let mut features = vec![
        // ⚠ The PLANE SENTINEL, pushed through the shared constant rather than spelled here: a
        // client (`vike_datahub_client::DatahubClient::connect_authed_on`) reads it to tell THIS
        // daemon — whose Write scope carries `Backfill` and `DeleteSeries` — from the compute one
        // before it signs anything. A literal that drifted from the client's copy would make this
        // daemon an unplaceable peer: still refused a compute-plane key, but refused to every
        // caller that asks for the data plane by name as well. The value is the frozen token it
        // always was — `"load_bars"`.
        DATA_PLANE_SENTINEL.to_string(),
        "scan_quotes".to_string(),
        "scan_trades".to_string(),
        "properties_as_of".to_string(),
        // PR-6 store-metadata verbs — served on EVERY build (HistStore trait methods, no DataFusion).
        "list_series".to_string(),
        "inventory".to_string(),
        "series_gaps".to_string(),
        // Their §6-Q2 sibling, likewise a trait verb on every build. UNCONDITIONAL, unlike
        // `backfill` below: there is no table to mount and nothing runtime about it, so the only
        // question its advertisement answers is "is this server older than the verb" — which is
        // precisely what the GUI's Partial column needs in order to choose between the wire answer
        // and an honest note.
        FEATURE_COVERAGE.to_string(),
        // The CHART-GAP SEED's CLASS field (`docs/decisions/0061` Phase 3). ⚠ **UNCONDITIONAL, and
        // deliberately NOT beside `FEATURE_SEED_SERIES` below**, although the two describe one
        // verb. That one is a RUNTIME fact — is this box's lane armed — and this one is a BUILD
        // fact, the `FEATURE_COVERAGE` shape: the only question its advertisement answers is "is
        // this daemon older than the field", and a build that decodes the field honours it whether
        // or not any lane is armed. Gating it on `has_seed` would make an unarmed-but-modern daemon
        // indistinguishable from an old one, and a client would then withhold a class from a server
        // that understands it perfectly well — and withholding it is the silent downgrade the field
        // exists to prevent. `FEATURE_SEED_CLASS`' own doc carries the three legs.
        FEATURE_SEED_CLASS.to_string(),
        // The SIX tick-level and research reads `docs/decisions/0084-only-the-datahub-touches-
        // the-store.md` asked this wire to grow. UNCONDITIONAL, the `FEATURE_COVERAGE` shape:
        // they are `vike_data::HistStore` trait verbs with no table to mount, so every build
        // that serves at all serves them and the advertisement answers exactly one question —
        // is this server older than the verb? Six strings rather than one family string;
        // `FEATURE_SCAN_BOOK_UPDATES`' own doc argues why.
        FEATURE_SCAN_BOOK_UPDATES.to_string(),
        FEATURE_SCAN_DEPTH.to_string(),
        FEATURE_SCAN_COHORT.to_string(),
        FEATURE_SCAN_PERP_METRICS.to_string(),
        FEATURE_SCAN_EQUITY.to_string(),
        FEATURE_SCAN_EXEC_FILLS.to_string(),
        // The ROW CAP on a range scan. UNCONDITIONAL like the six above and for the same reason —
        // it is a property of this build's `handle_request`, not of a mounted table — and the
        // advertisement is load-bearing rather than informational: an older server IGNORES an
        // unknown `limit` field (serde skips unknown fields) and answers with EVERY row, so a
        // client that assumed the cap without checking would ask for a page and get a frame that
        // overruns `MAX_FRAME_LEN`. Silence here means "do not send one".
        FEATURE_SCAN_LIMIT.to_string(),
        // 0084's SEVENTH verb — the one that record did not price, because it measured the
        // `HistStore` trait and this was inherent to `DataFusionHist`. Unconditional, the
        // `FEATURE_COVERAGE` shape.
        FEATURE_SERIES_FACTS.to_string(),
        // The HISTORY-CHANNELS read (`docs/decisions/0102`). UNCONDITIONAL, the `FEATURE_COVERAGE`
        // shape and deliberately NOT `FEATURE_BACKFILL`'s per-mounted-table rule below: whether a
        // lane is mounted travels INSIDE the answer, so a table-less build still serves the verb,
        // and gating the string on the table would make an unmounted modern server look like an
        // old one. The advertisement answers one question — is this server older than the verb.
        FEATURE_HISTORY_CHANNELS.to_string(),
    ];
    // The backfill-on-demand verb — advertised per MOUNTED TABLE, not per cfg (see the doc above).
    // This advertisement is the verb's whole negotiation: it shipped without a PROTO_VERSION bump.
    if has_backfill {
        features.push(FEATURE_BACKFILL.to_string());
        // ...and the operator's door onto the backfills it runs, on the SAME condition: the
        // registry `ListBackfills` reads and `CancelBackfill` flags lives with the mounted table,
        // and a server with none runs nothing to list or stop. Not tied to keys — a key-less
        // loopback server serves both, as it serves `Backfill`
        // (`docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`).
        features.push(FEATURE_BACKFILL_CANCEL.to_string());
    }
    // ...and its FUNDING lane, per mounted funding SOURCE rather than per table: a table with none
    // can only refuse `interval=funding`, and a server older than the lane refuses it as an
    // unmeasurable step — so silence here is what makes a client refuse first, sending nothing
    // (`FEATURE_BACKFILL_FUNDING`'s own doc).
    if has_backfill_funding {
        features.push(FEATURE_BACKFILL_FUNDING.to_string());
    }
    // The CHART-GAP SEED lane — advertised per ARMED LANE, a runtime fact like `backfill`'s
    // per-mounted-table rule. `has_seed` is `SeedLane`'s mere existence, and that is deliberate:
    // `crate::datahub_cli` builds one only under `VIKE_DATAHUB_CHART_SEED=1`, so there is no
    // `enabled: bool` for this advertisement to get out of step with.
    //
    // ⚠ Its absence does NOT make the verb refuse, which is the one place this capability differs
    // from every other on this wire — an unarmed server answers `SeedDone { armed: false }` and
    // writes nothing. `FEATURE_SEED_SERIES`' own doc argues why that difference IS the Observe
    // classification rather than a leniency beside it, and
    // `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` puts removing it in the reopen
    // list. The advertisement is still what stops a well-behaved client sending, so an operator
    // learns which switch is off instead of watching a chart that never fills.
    if has_seed {
        features.push(FEATURE_SEED_SERIES.to_string());
    }
    // The VENUE-CATALOG lane — advertised per ARMED LANE, the same runtime rule as `seed_series`
    // above, and `has_catalog` is `CatalogLane`'s mere existence for the same reason (there is no
    // `enabled: bool` for the advertisement to get out of step with).
    //
    // ⚠ Its absence does NOT make the verb refuse either: an unarmed server answers
    // `CatalogOutcome::NotArmed` and calls no venue. What the advertisement buys here is
    // specifically the thing an EMPTY LIST would destroy — a client that knows the lane is unarmed
    // says so, where a client that merely got no instruments could not tell that apart from `ig`
    // and `ibkr`, which genuinely have none (`docs/decisions/0062`'s decision 5).
    if has_catalog {
        features.push(FEATURE_VENUE_CATALOG.to_string());
    }
    // The ARCHIVE IMPORT lane — advertised per MOUNTED LANE, the `backfill` rule, and the
    // capability rides TOGETHER with one `import_format=<id>` entry per registered format. A daemon
    // with no project above it, or a build without the format registry, mounts none and pushes
    // nothing here (`crate::import::mount`), so "can serve" and "will serve" stay one answer.
    features.extend(crate::import::advertised(import));
    // The MARKET-DATA plane, advertised per MOUNTED HUB — a RUNTIME fact exactly like `backfill`'s
    // per-mounted-table rule, NOT a build fact like `coverage`. A binary carrying `live-feeds` whose
    // operator did not set `VIKE_DATAHUB_LIVE=1` mounts no hub and advertises nothing, which is what
    // keeps "can serve" and "will serve" one answer.
    //
    // ⚠ The per-venue entries ride BESIDE the named capability rather than replacing it, and they are
    // collision-safe by construction: every capability check in this protocol family is whole-string
    // equality, so an `md_venue=binance` entry can neither satisfy nor shadow a named capability.
    // They are what lets a client learn at the HANDSHAKE which venues it may name, instead of one
    // `VenueNotServed` refusal at a time.
    if let Some(hub) = md {
        features.push(FEATURE_MARKET_DATA.to_string());
        for venue in hub.served_venues() {
            features.push(md_venue_feature(venue));
        }
    }
    // THE RECORDING plane's venue roster — `rec_venue=<slug>`, one entry per venue this build can
    // RECORD, and the twin of the `md_venue=` block directly above.
    //
    // ⚠ **A BUILD fact, like `FEATURE_COVERAGE` and UNLIKE the `md_venue=` entries it sits beside.**
    // The difference is deliberate and it follows the QUESTION each answers. `md_venue=` answers
    // "will this process serve me a live tick", which is false without a mounted hub however the
    // binary was compiled — a runtime fact. This one answers "if I write `okx` into a subscription
    // row, will the daemon refuse to start at its next restart", and that is decided by
    // `crate::recording::build_recording_feed`'s compiled arms alone: a serve-only invocation of a
    // `record-binance` build still could not record `okx`, and could record `binance` the moment an
    // operator adds the flag. Gating this on the `--record` flag would make the advertisement
    // answer a different question from the one a client asks it.
    //
    // ⚠ A build with NO recording plane pushes NOTHING here, and `FEATURE_REC_VENUE_PREFIX`'s own
    // doc carries what a client must do about it: an empty advertisement is AMBIGUOUS (an old
    // server, or a `record`-less build) and is therefore the same answer as an unreachable server —
    // warn and proceed, never refuse. The refusal is available only where the server advertised at
    // least one venue.
    #[cfg(feature = "record")]
    for venue in crate::recording::supported() {
        features.push(vike_datahub_client::proto::rec_venue_feature(venue));
    }
    // ⚠ The AUTH advertisement, and it must be LAST-but-conditional in exactly this way: a key-less
    // server never pushes it, so its whole `Welcome` — features included — is byte-identical to the
    // pre-auth protocol's. That identity is the backward-compatibility contract, not a nicety, and
    // `a_keyless_servers_welcome_is_byte_identical_to_the_pre_auth_protocol` is what holds it.
    // On a KEYED server this string is how a client learns authentication is mandatory here BEFORE
    // it sends a verb it would only be refused for.
    if requires_auth {
        features.push(FEATURE_AUTH.to_string());
        // ⚠ The DESTRUCTIVE verb rides the SAME conditional, and that pairing is the whole posture:
        // it is served exactly when the server can enforce a scope, and never otherwise. A key-less
        // server therefore does not advertise it AND refuses it (`delete_series_verb`) — the
        // advertisement is what stops a well-behaved client sending one, and the refusal is what
        // holds for a client that does. Neither substitutes for the other.
        features.push(FEATURE_DELETE_SERIES.to_string());
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The `rec_venue=` advertisement is exactly what THIS BUILD can record — both ways.**
    ///
    /// ⚠ It is the only thing that proves the push in [`served_features`] happens at all. The
    /// prefix, the builder and the reader are unit-tested in the light crate
    /// (`crates/vike-datahub-client/src/proto_tests.rs`'s `the_rec_venue_feature_round_trips`), but a
    /// round trip in that crate says nothing about whether a server ever WRITES one — and a read
    /// half with no write half is worse than neither, because a client that refuses an
    /// unadvertised venue would then refuse every venue against a server that records it perfectly
    /// well.
    ///
    /// ⚠ **Both configurations are asserted and neither is vacuous.** The DEFAULT build carries no
    /// recording plane, so it must advertise NOTHING — that arm runs in the derived roster lane on
    /// every PR. A `record-*` build must advertise exactly `crate::recording::supported()`, and
    /// that arm runs in `cargo test -p vike-datahub --features
    /// record-polymarket,record-binance`, the lane `scripts/ci_feature_suite.sh`'s
    /// `recorder-venues` arm spells for the recorder's venue feeds.
    #[test]
    fn the_recordable_venues_are_advertised_as_this_build_can_record_them() {
        let features = served_features(false, false, false, None, false, false, None);
        let advertised = vike_datahub_client::proto::advertised_rec_venues(&features);

        #[cfg(feature = "record")]
        {
            let can_record: Vec<String> =
                crate::recording::supported().into_iter().map(str::to_string).collect();
            assert_eq!(
                advertised, can_record,
                "the advertisement must BE `crate::recording::supported()`, in order — a \
                 client refuses a `record add` for any venue it does not see here"
            );
        }
        #[cfg(not(feature = "record"))]
        assert!(
            advertised.is_empty(),
            "a build with no recording plane records nothing and must advertise nothing — an \
             empty advertisement is AMBIGUOUS by design and a client degrades on it: {advertised:?}"
        );

        // …and it never collides with the LIVE plane's entries, whatever this build carries.
        assert!(
            vike_datahub_client::advertised_md_venues(&features).is_empty(),
            "no md hub was mounted, so no `md_venue=` entry may appear: {features:?}"
        );
    }
}
