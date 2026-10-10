//! `rosters.json` — the five remaining CI-gated rosters.
//! - `rosters.json` — five rosters, four of them read straight off their authority:
//!   `vike_model::venues::venue_caps`'s `ORDER_KINDS`/`TRIGGER_KINDS`, `vike_model::AssetClass` (with
//!   each class's Symbol-picker tab, through `vike_catalog::tab_for`), `vike_data::store::store_kind::
//!   STORE_KINDS` verbatim, and `vike_exec::recon`'s `DivergenceKind::ALL`. The divergence roster's
//!   per-policy verdicts are **COMPUTED** through `vike_exec::recon::mode_applies` rather than
//!   restated — see [`divergence_record`], which carries why. The fifth, [`EVENTS`], is the one
//!   DECLARED table here besides [`WIRING`](crate::WIRING): a wire tag is a serde ATTRIBUTE no
//!   runtime renderer can read, so it is declared and pinned against the enum's source by the
//!   gate.

use serde_json::{Value, json};
use vike_data::store::store_kind::{Partition, STORE_KINDS, StoreKind};
use vike_exec::recon::{DivergenceKind, DivergenceOrigin, POLICY_NAMES, ReconPolicy, mode_applies};
use vike_model::AssetClass;
use vike_model::venues::venue_caps::{ORDER_KINDS, TRIGGER_KINDS};

/// Every `vike_model::events::Event` variant, as `(variant, payload type, wire tag)`.
///
/// DECLARED here rather than derived, and that is forced: the wire tag is a `#[serde(rename = …)]`
/// ATTRIBUTE, which no runtime renderer can read, and the enum has no value this module could
/// enumerate. The declaration is therefore PINNED against its authority the way
/// [`P99_BUDGET_NS`](crate::P99_BUDGET_NS) is: `docs_data_gate.rs`'s
/// `event_table_matches_the_event_enum` parses
/// `crates/vike-model/src/events.rs`'s enum body — every variant, its payload type, and the rename
/// where one exists — and fails when this table and that source disagree, so a new variant reddens
/// CI until it is listed.
///
/// Two variants carry a rename because their wire tag predates the Rust spelling: `Fill` ships as
/// `"FillEvent"` and `Funding` as `"FundingEvent"` — fixtures and the exec journal pin those tags,
/// so only the Rust variant was shortened.
pub const EVENTS: &[(&str, &str, &str)] = &[
    ("Fill", "FillEvent", "FillEvent"),
    ("OrderSubmitted", "OrderSubmitted", "OrderSubmitted"),
    ("OrderAccepted", "OrderAccepted", "OrderAccepted"),
    ("OrderRejected", "OrderRejected", "OrderRejected"),
    ("OrderDenied", "OrderDenied", "OrderDenied"),
    ("OrderTriggered", "OrderTriggered", "OrderTriggered"),
    ("OrderPartiallyFilled", "OrderPartiallyFilled", "OrderPartiallyFilled"),
    ("OrderFilled", "OrderFilled", "OrderFilled"),
    ("OrderCanceled", "OrderCanceled", "OrderCanceled"),
    ("OrderExpired", "OrderExpired", "OrderExpired"),
    ("OrderLiquidated", "OrderLiquidated", "OrderLiquidated"),
    ("OrderModified", "OrderModified", "OrderModified"),
    ("PositionOpened", "PositionOpened", "PositionOpened"),
    ("PositionChanged", "PositionChanged", "PositionChanged"),
    ("PositionClosed", "PositionClosed", "PositionClosed"),
    ("AccountState", "AccountState", "AccountState"),
    ("Funding", "FundingEvent", "FundingEvent"),
    ("PositionLiquidated", "PositionLiquidated", "PositionLiquidated"),
    ("OrderCancelRejected", "OrderCancelRejected", "OrderCancelRejected"),
    ("OrderModifyRejected", "OrderModifyRejected", "OrderModifyRejected"),
];

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
    }
}

/// Kebab-case id of a [`DivergenceKind`] — the slug a per-kind page is keyed on.
const fn divergence_kind_name(kind: DivergenceKind) -> &'static str {
    match kind {
        DivergenceKind::MissingFill => "missing-fill",
        DivergenceKind::MissingTerminal => "missing-terminal",
        DivergenceKind::OrphanLocalOrder => "orphan-local-order",
        DivergenceKind::UnknownOrder => "unknown-order",
        DivergenceKind::PositionDrift => "position-drift",
        DivergenceKind::PositionOnlyExternal => "position-only-external",
        DivergenceKind::OrphanLocalPosition => "orphan-local-position",
        DivergenceKind::BalanceDrift => "balance-drift",
        DivergenceKind::JournalDivergence => "journal-divergence",
    }
}

/// Kebab-case name of a [`DivergenceOrigin`] — where the EVIDENCE for a divergence came from, the
/// axis `external-quarantine` keys on.
const fn divergence_origin_name(origin: DivergenceOrigin) -> &'static str {
    match origin {
        DivergenceOrigin::ReconciliationMaterialized => "reconciliation-materialized",
        DivergenceOrigin::External => "external",
        DivergenceOrigin::Absence => "absence",
    }
}

/// The kinds whose resolution produces real events — the candidate set
/// `vike_tradehub::reconcile_config::auto_applied_kinds` filters through `mode_applies`. Every other
/// kind folds nothing whatever its policy verdict, which is the distinction
/// [`divergence_record`]'s doc exists to keep visible.
const RESOLVING_KINDS: [DivergenceKind; 5] = [
    DivergenceKind::MissingFill,
    DivergenceKind::PositionDrift,
    DivergenceKind::UnknownOrder,
    DivergenceKind::PositionOnlyExternal,
    DivergenceKind::BalanceDrift,
];

/// One divergence kind's record: its id, its origin class, and what each of the four policies does
/// with it.
///
/// The per-policy answer is **COMPUTED** through `vike_exec::recon::mode_applies`, never restated.
/// That is the whole reason this roster exists: the workspace's own guidance records that the
/// opposite claim about `hybrid` was written down in six places and was false in all six, and the
/// cure it names is asking the function rather than copying a list. `"fold"` means the policy
/// auto-applies the kind; `"hold"` means it is quarantined for an operator.
///
/// ⚠ `fold` is not the same as "does something": `OrphanLocalOrder` and `MissingTerminal` fold
/// under `hybrid` and resolve to EMPTY event lists, so they change nothing and do not even alert.
/// The `resolves_to_events` flag carries that distinction, from the same candidate set
/// (`RESOLVING_KINDS`) `vike_tradehub::reconcile_config::auto_applied_kinds` filters.
fn divergence_record(kind: DivergenceKind) -> Value {
    let policies: serde_json::Map<String, Value> = POLICY_NAMES
        .iter()
        .map(|&name| {
            let policy = ReconPolicy::from_policy_name(name).unwrap_or_else(|| {
                panic!("POLICY_NAMES lists {name}, which ReconPolicy::from_policy_name refuses")
            });
            let verdict = if mode_applies(&policy, kind) { "fold" } else { "hold" };
            (name.to_string(), Value::from(verdict))
        })
        .collect();
    json!({
        "id": divergence_kind_name(kind),
        "variant": format!("{kind:?}"),
        "origin": divergence_origin_name(kind.origin()),
        "resolves_to_events": RESOLVING_KINDS.contains(&kind),
        "policies": Value::Object(policies),
    })
}

/// How a store kind's series leaf is partitioned, kebab-case.
const fn partition_name(partition: Partition) -> &'static str {
    match partition {
        Partition::Symbol => "symbol",
        Partition::SymbolInterval => "symbol-interval",
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
/// `events` is the declared-and-pinned [`EVENTS`] table (see its doc for why a renderer cannot
/// derive it); `order_kinds` is `vike_model::venues::venue_caps`'s `ORDER_KINDS` with its `TRIGGER_KINDS`
/// subset; `asset_classes` is [`AssetClass`] with each class's Symbol-picker tab; `store_kinds` is
/// `vike_data::store::store_kind::STORE_KINDS` verbatim; `divergence_kinds` is every
/// `vike_exec::recon::DivergenceKind::ALL` member, in that order, with its origin and its four
/// COMPUTED policy verdicts.
///
/// ⚠ `commit_keys` inside a store kind is OBSERVED, not exhaustive, and `StoreKind::commit_keys`'s
/// own doc explains why no gate can make it so: a commit key is an ordinary `&str` argument. What
/// IS gated per listed entry is that the producer file exists and spells the template verbatim. A
/// consumer must not render it as "the producers of this kind".
#[must_use]
pub fn rosters_value() -> Value {
    json!({
        "events": EVENTS
            .iter()
            .map(|(variant, payload, wire_tag)| json!({
                "variant": variant,
                "payload": payload,
                "wire_tag": wire_tag,
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
        "divergence_kinds": DivergenceKind::ALL
            .iter()
            .map(|&k| divergence_record(k))
            .collect::<Vec<_>>(),
    })
}
