//! `rosters.json` — the five remaining CI-gated rosters.
//! - `rosters.json` — five rosters, all read straight off their authority:
//!   `vike_model::events::Event::TABLE` (every event variant with its payload type and wire tag,
//!   expanded from the same `events!` list as the enum, so a serde rename cannot disagree with it),
//!   `vike_venues::venue_caps`'s `ORDER_KINDS`/`TRIGGER_KINDS`, `vike_model::AssetClass` (with
//!   each class's Symbol-picker tab, through `vike_catalog::tab_for`), `vike_data::store::store_kind::
//!   STORE_KINDS` verbatim, and `vike_execution::recon`'s `ReconcileDivergenceKind::ALL`. The divergence roster's
//!   per-policy verdicts are **COMPUTED** through `vike_execution::recon::mode_applies` rather than
//!   restated — see [`divergence_record`], which carries why. Nothing in this crate is DECLARED: every
//!   roster is read off its authority.

use serde_json::{Value, json};
use vike_data::store::store_kind::{STORE_KINDS, StoreKind, StoreKindPartition};
use vike_execution::recon::{
    POLICY_NAMES, ReconcileDivergenceKind, ReconcileDivergenceOrigin, ReconcilePolicy, mode_applies,
};
use vike_model::AssetClass;
use vike_model::events::Event;
use vike_venues::venue_caps::{ORDER_KINDS, TRIGGER_KINDS};

/// Kebab-case id of an [`AssetClass`].
///
/// ⚠ `Index` renders `index-instrument`, NOT `index`: every consumer of this roster keys a page on
/// the id, and `index` is the reserved slug of a directory's own index page — a class rendered as
/// `index` would silently overwrite the tree's landing page. The docs architecture states the same
/// exception from the docs side; this is where it is enforced.
const fn asset_class_name(class: AssetClass) -> &'static str {
    match class {
        AssetClass::Equity => "equity",
        AssetClass::Etf => "etf",
        AssetClass::CryptoSpot => "crypto-spot",
        AssetClass::CryptoPerp => "crypto-perp",
        AssetClass::CryptoFuture => "crypto-future",
        AssetClass::Option => "option",
        AssetClass::Fx => "fx",
        AssetClass::Future => "future",
        AssetClass::Index => "index-instrument",
        AssetClass::PredictionMarket => "prediction-market",
        AssetClass::Cfd => "cfd",
        AssetClass::Bond => "bond",
        AssetClass::Fund => "fund",
        AssetClass::Warrant => "warrant",
        AssetClass::Commodity => "commodity",
        AssetClass::Combo => "combo",
    }
}

/// Kebab-case id of a [`ReconcileDivergenceKind`] — the slug a per-kind page is keyed on.
const fn divergence_kind_name(kind: ReconcileDivergenceKind) -> &'static str {
    match kind {
        ReconcileDivergenceKind::MissingFill => "missing-fill",
        ReconcileDivergenceKind::MissingTerminal => "missing-terminal",
        ReconcileDivergenceKind::OrphanLocalOrder => "orphan-local-order",
        ReconcileDivergenceKind::UnknownOrder => "unknown-order",
        ReconcileDivergenceKind::PositionDrift => "position-drift",
        ReconcileDivergenceKind::PositionOnlyExternal => "position-only-external",
        ReconcileDivergenceKind::OrphanLocalPosition => "orphan-local-position",
        ReconcileDivergenceKind::BalanceDrift => "balance-drift",
        ReconcileDivergenceKind::JournalDivergence => "journal-divergence",
        ReconcileDivergenceKind::OrderTermsDrift => "order-terms-drift",
    }
}

/// Kebab-case name of a [`ReconcileDivergenceOrigin`] — where the EVIDENCE for a divergence came from, the
/// axis `external-quarantine` keys on.
const fn divergence_origin_name(origin: ReconcileDivergenceOrigin) -> &'static str {
    match origin {
        ReconcileDivergenceOrigin::ReconciliationMaterialized => "reconciliation-materialized",
        ReconcileDivergenceOrigin::External => "external",
        ReconcileDivergenceOrigin::Absence => "absence",
    }
}

/// One divergence kind's record: its id, its origin class, and what each of the four policies does
/// with it.
///
/// The per-policy answer is **COMPUTED** through `vike_execution::recon::mode_applies`, never restated.
/// That is the whole reason this roster exists: the workspace's own guidance records that the
/// opposite claim about `hybrid` was written down in six places and was false in all six, and the
/// cure it names is asking the function rather than copying a list. `"fold"` means the policy
/// auto-applies the kind; `"hold"` means it is quarantined for an operator.
///
/// ⚠ `fold` is not the same as "does something": `OrphanLocalOrder` and `MissingTerminal` fold
/// under `hybrid` and resolve to EMPTY event lists, so they change nothing and do not even alert.
/// The `resolves_to_events` flag carries that distinction, read from the same
/// `ReconcileDivergenceKind::resolves_to_events` `vike_tradehub::reconcile_config::auto_applied_kinds`
/// filters.
fn divergence_record(kind: ReconcileDivergenceKind) -> Value {
    let policies: serde_json::Map<String, Value> = POLICY_NAMES
        .iter()
        .map(|&name| {
            let policy = ReconcilePolicy::from_policy_name(name).unwrap_or_else(|| {
                panic!("POLICY_NAMES lists {name}, which ReconcilePolicy::from_policy_name refuses")
            });
            let verdict = if mode_applies(&policy, kind) { "fold" } else { "hold" };
            (name.to_string(), Value::from(verdict))
        })
        .collect();
    json!({
        "id": divergence_kind_name(kind),
        "variant": format!("{kind:?}"),
        "origin": divergence_origin_name(kind.origin()),
        "resolves_to_events": kind.resolves_to_events(),
        "policies": Value::Object(policies),
    })
}

/// How a store kind's series leaf is partitioned, kebab-case.
const fn partition_name(partition: StoreKindPartition) -> &'static str {
    match partition {
        StoreKindPartition::Symbol => "symbol",
        StoreKindPartition::SymbolInterval => "symbol-interval",
    }
}

/// One `kind=` of the hist store, rendered from its [`StoreKind`] row verbatim — the layout
/// contract its producers and consumers must agree on.
fn store_kind_record(k: &StoreKind) -> Value {
    json!({
        "kind": k.kind,
        "row": k.row,
        "codec": k.codec,
        "write_verb": k.write_verb,
        "read_verb": k.read_verb,
        "columns": k
            .columns
            .iter()
            .map(|(name, flavor)| json!({ "name": name, "flavor": flavor }))
            .collect::<Vec<_>>(),
        "schema_meta": k.schema_meta,
        "partition": partition_name(k.partition),
        "grouped": k.grouped,
        "tick_lane": k.tick_lane,
        "identity": k.identity,
        "commit_keys": k
            .commit_keys
            .iter()
            .map(|c| json!({ "producer": c.producer, "template": c.template }))
            .collect::<Vec<_>>(),
        "notes": k.notes,
    })
}

/// The whole `rosters.json` document: the five remaining CI-gated rosters the docs render a page
/// per member from, each from the table the runtime consults.
///
/// `events` is `vike_model::events::Event::TABLE`, expanded from the declaration of the enum itself,
/// so the wire tags it prints are the serde renames the wire carries; `order_kinds` is `vike_venues::venue_caps`'s `ORDER_KINDS` with its `TRIGGER_KINDS`
/// subset; `asset_classes` is [`AssetClass`] with each class's Symbol-picker tab; `store_kinds` is
/// `vike_data::store::store_kind::STORE_KINDS` verbatim; `divergence_kinds` is every
/// `vike_execution::recon::ReconcileDivergenceKind::ALL` member, in that order, with its origin and its four
/// COMPUTED policy verdicts.
///
/// ⚠ `commit_keys` inside a store kind is OBSERVED, not exhaustive, and `StoreKind::commit_keys`'s
/// own doc explains why no gate can make it so: a commit key is an ordinary `&str` argument. What
/// IS gated per listed entry is that the producer file exists and spells the template verbatim. A
/// consumer must not render it as "the producers of this kind".
#[must_use]
pub fn rosters_value() -> Value {
    json!({
        "events": Event::TABLE
            .iter()
            .map(|row| json!({
                "variant": row.variant,
                "payload": row.payload,
                "wire_tag": row.wire_tag,
            }))
            .collect::<Vec<_>>(),
        "order_kinds": {
            "all": ORDER_KINDS,
            "trigger": TRIGGER_KINDS,
        },
        "asset_classes": AssetClass::ALL
            .iter()
            .map(|&c| json!({
                "id": asset_class_name(c),
                "variant": format!("{c:?}"),
                "picker_tab": vike_catalog::tab_for(c).label(),
            }))
            .collect::<Vec<_>>(),
        "store_kinds": STORE_KINDS.iter().map(store_kind_record).collect::<Vec<_>>(),
        "divergence_kinds": ReconcileDivergenceKind::ALL
            .iter()
            .map(|&k| divergence_record(k))
            .collect::<Vec<_>>(),
    })
}
