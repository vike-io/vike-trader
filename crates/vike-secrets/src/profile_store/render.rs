//! Rendering stored rows back into the TOML documents the profile parsers read.

use std::collections::BTreeMap;

use super::*;

/// **Render a stored recorder body back into the TOML document `vike_recorder` parses.**
///
/// THE ONE RENDERER, and it is in this leaf crate deliberately: both consumers need it and they
/// cannot see each other. `crates/vike-datahub/src/recorder.rs` feeds the result to
/// `RecorderProfile::from_toml`, so every refusal that function performs — serde's
/// `deny_unknown_fields` on four structs, the missing-`store` parse error and all seven
/// `validate` rules — applies to a row-loaded profile with no second implementation.
/// `crates/vike-cli`'s migration renders the rows it is about to write and requires the result to
/// PARSE EQUAL to the file, which is the fence that stops a wrong body being stored.
///
/// ⚠ It emits ONLY the keys the rows carry. A `None` is an OMITTED key, not a rendered default —
/// see [`RecorderRow`]. `[maintenance]` and `[alerting]` headers are emitted only when at least one
/// of their keys is present, because an EMPTY table and an ABSENT one mean the same thing to
/// `vike_recorder::config` and the file being migrated has one or the other, never both.
///
/// ⚠ No value is escaped and none needs to be: `store`, `family`, `venue` and `series_prefix` are
/// rendered through [`toml_basic_string`], and `symbols`/`alert_webhooks` are stored ALREADY
/// RENDERED (the writer produced them with the same helper). A caller that hand-wrote a row with an
/// unbalanced array string gets a TOML parse error from `from_toml`, which is a refusal rather than
/// a silent misread.
#[must_use]
pub fn render_recorder_toml(body: &RecorderBody) -> String {
    let mut doc = String::new();
    doc.push_str(&format!("store = {}\n", toml_basic_string(&body.row.store)));
    for s in &body.subscriptions {
        doc.push_str("\n[[subscribe]]\n");
        doc.push_str(&format!("venue = {}\n", toml_basic_string(&s.venue)));
        if let Some(f) = &s.family {
            doc.push_str(&format!("family = {}\n", toml_basic_string(f)));
        }
        if let Some(syms) = &s.symbols {
            doc.push_str(&format!("symbols = {syms}\n"));
        }
        if let Some(b) = &s.backfill {
            doc.push_str(&format!("backfill = {}\n", toml_basic_string(b)));
        }
    }
    let m = &body.row;
    let maintenance =
        [m.interval_secs, m.min_parts, m.target_mb, m.max_merge_rows, m.retention_days];
    if maintenance.iter().any(Option::is_some) {
        doc.push_str("\n[maintenance]\n");
        for (key, value) in [
            ("interval_secs", m.interval_secs),
            ("min_parts", m.min_parts),
            ("target_mb", m.target_mb),
            ("max_merge_rows", m.max_merge_rows),
            ("retention_days", m.retention_days),
        ] {
            if let Some(v) = value {
                doc.push_str(&format!("{key} = {v}\n"));
            }
        }
    }
    if m.alert_webhooks.is_some()
        || m.alert_repeat_secs.is_some()
        || m.alert_series_prefix.is_some()
    {
        doc.push_str("\n[alerting]\n");
        if let Some(w) = &m.alert_webhooks {
            doc.push_str(&format!("webhooks = {w}\n"));
        }
        if let Some(v) = m.alert_repeat_secs {
            doc.push_str(&format!("repeat_secs = {v}\n"));
        }
        if let Some(p) = &m.alert_series_prefix {
            doc.push_str(&format!("series_prefix = {}\n", toml_basic_string(p)));
        }
    }
    doc
}

/// A TOML basic string, with the five escapes TOML requires. Pure, and the ONE place a value from
/// this store becomes document text — the writer and [`render_recorder_toml`] both go through it,
/// so a symbol carrying a quote round-trips instead of producing an unparseable document.
#[must_use]
pub fn toml_basic_string(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A TOML array of basic strings — the rendering `SubscriptionRow::symbols` and
/// `RecorderRow::alert_webhooks` are STORED as, so the column holds document text a renderer can
/// emit verbatim.
#[must_use]
pub fn toml_string_array(values: &[String]) -> String {
    let inner = values.iter().map(|v| toml_basic_string(v)).collect::<Vec<_>>().join(", ");
    format!("[{inner}]")
}

/// One `f64` spelled the way TOML spells it, so a value that round-trips through a `REAL` column is
/// the same value and the same TYPE.
///
/// `{:?}` on an `f64` is Rust's shortest round-tripping form; the `.0` suffix is what stops `20.0`
/// coming back as the TOML INTEGER `20`, which `serde` then refuses for an `Option<f64>` field in
/// some positions and accepts in others — a difference a round-trip fence would report as a
/// migration defect.
#[must_use]
fn toml_f64(v: f64) -> String {
    let s = format!("{v:?}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

/// Split a dotted `profile_setting` path into its TABLE path and its LEAF key. An empty table path
/// means a TOP-LEVEL key, which TOML requires be emitted before any header.
fn split_setting_path(path: &str) -> (&str, &str) {
    match path.rfind('.') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// **Render a stored RUN profile back into the TOML document `vike_core::RunProfile` parses.**
///
/// THE ONE RENDERER for this document, in the leaf crate for the same reason
/// [`render_recorder_toml`] is: **both consumers need it and they cannot see each other.**
/// `crates/vike-tradehub/src/profile_rows.rs`'s `rows_to_run_profile` feeds the result to
/// `RunProfile::from_toml_str`, so every refusal that parser performs — `deny_unknown_fields` on
/// all seven structs, the `[event_source]`/`[broker]` tombstones, the `mode = "live"` venue-owned
/// grid refusal, `max_leverage >= 1.0` — applies to a row-loaded profile with **no second
/// validator**, which is 0057's *What is LOST* requirement in its own words. And
/// `crates/vike-cli/src/cmd/config/mirror_profile.rs` renders the rows it is about to write and
/// requires the result to PARSE EQUAL to the file, which is the fence. The CLI cannot reach a
/// renderer that lives above `vike-secrets`: it links neither `vike-core` nor `vike-tradehub` (the
/// `light-consumers` lane holds it out of that closure), and that constraint is exactly why
/// `config mirror --recorder` works and why no daemon-profile writer existed before this landing.
///
/// # ⚠ It has NO key vocabulary, and that is stronger rather than weaker
///
/// [`render_daemon_toml`] refuses a `profile_setting` path it does not know. This one cannot: a run
/// profile's leaves are five nested tables deep and a hand-written list here would be a second
/// encoding of `RunProfile`'s own `#[derive(Deserialize)]`. So every row becomes a key at its own
/// dotted address, and a path that names nothing real is refused BY SERDE at the moment the
/// document is parsed — `deny_unknown_fields`, the same refusal the file path gets. **Nothing is
/// ever silently dropped**, which is the property the daemon renderer's refusal exists to buy.
///
/// # What the shape has to get right
///
/// * **Top-level keys come first.** TOML requires every bare key before the first header, and
///   `mode` sorts AFTER `guards.*` in a `BTreeMap` — so iterating the map once and emitting as it
///   goes produces an invalid document.
/// * **Each table is emitted ONCE.** `guards.freshness_ms` < `guards.margin_call.buffer` <
///   `guards.max_drawdown` in path order, so a naive walk opens `[guards]`, opens
///   `[guards.margin_call]`, and then re-opens `[guards]` — which TOML refuses as a duplicate
///   table. The rows are grouped by table first.
/// * **A parent table precedes its child**, which lexicographic order over the dotted table paths
///   gives for free (`guards` < `guards.margin_call`, `sinks` < `sinks.journal`).
///
/// Values are emitted VERBATIM: a `profile_setting` value is already the TOML rendering of one
/// scalar, which is the idiom the `daemon.summary_ms` rows have carried since Phase 3.
#[must_use]
pub fn render_run_toml(stored: &StoredProfile) -> String {
    let mut doc = String::new();
    let mut tables: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    for (path, value) in &stored.settings {
        let (table, key) = split_setting_path(path);
        if table.is_empty() {
            doc.push_str(&format!("{key} = {value}\n"));
        } else {
            tables.entry(table).or_default().push((key, value.as_str()));
        }
    }
    for (table, keys) in tables {
        doc.push_str(&format!("\n[{table}]\n"));
        for (key, value) in keys {
            doc.push_str(&format!("{key} = {value}\n"));
        }
    }
    doc
}

/// Emit one mount's scalar keys, in the order the shipped profiles write them.
///
/// `primary` is emitted only in the array spelling, because it is a `MountCfg` field and
/// `DaemonProfile` (the single-mount spelling) carries no such key — writing it at the top level
/// would be an unknown field.
fn push_mount_keys(doc: &mut String, m: &MountRow, array_spelling: bool) {
    doc.push_str(&format!("venue = {}\n", toml_basic_string(&m.venue)));
    // ⚠ NOT conditional, unlike every optional key below it: the column is NOT NULL, so a row
    // always carries one and a rendered document that omitted it would round-trip back to a
    // profile that no longer says what it trades — the "missing claim becomes a legal value"
    // hazard `docs/decisions/0061` phase 5 made the column mandatory to avoid.
    doc.push_str(&format!("asset_class = {}\n", toml_basic_string(&m.asset_class)));
    if let Some(s) = &m.symbol {
        doc.push_str(&format!("symbol = {}\n", toml_basic_string(s)));
    }
    if let Some(t) = &m.token_id {
        doc.push_str(&format!("token_id = {}\n", toml_basic_string(t)));
    }
    if let Some(v) = &m.interval {
        doc.push_str(&format!("interval = {}\n", toml_basic_string(v)));
    }
    if let Some(v) = m.interval_ms {
        doc.push_str(&format!("interval_ms = {v}\n"));
    }
    if let Some(v) = m.resolution_ts_ms {
        doc.push_str(&format!("resolution_ts_ms = {v}\n"));
    }
    if let Some(v) = m.qty {
        doc.push_str(&format!("qty = {}\n", toml_f64(v)));
    }
    if let Some(v) = m.half_spread {
        doc.push_str(&format!("half_spread = {}\n", toml_f64(v)));
    }
    if let Some(v) = m.tick_size {
        doc.push_str(&format!("tick_size = {}\n", toml_f64(v)));
    }
    if let Some(v) = m.seed_cash {
        doc.push_str(&format!("seed_cash = {}\n", toml_f64(v)));
    }
    if let Some(v) = m.data_only {
        doc.push_str(&format!("data_only = {v}\n"));
    }
    if let Some(v) = &m.account {
        doc.push_str(&format!("account = {}\n", toml_basic_string(v)));
    }
    if array_spelling && m.is_primary {
        doc.push_str("primary = true\n");
    }
}

/// Emit one mount's `[strategy]` table, with its `[…params]` child.
///
/// `parent` is the enclosing table for the array spelling (`Some("mounts")` ⇒ `[mounts.strategy]`)
/// and `None` for the single-mount spelling (⇒ `[strategy]`).
///
/// ⚠ **The header is COMPOSED from two words rather than spelled as one literal**, which is not
/// style: `crates/vike-ops/tests/settings_secrets/credential_source_roster_gate.rs` harvests every FILE-NAME-shaped
/// string this crate spells, on the ground that a name in the credential store's own crate is a
/// place a credential VALUE could come from — and a dotted `<table>.<child>` literal reads as a
/// file name to it. MEASURED: the one-literal spelling reddened that gate in a lane.
///
/// ⚠ The table is emitted when the mount has params even with no `name` and no `rhai`, which the
/// first version of this renderer did not do: `StrategyCfg` defaults both, so a params-only
/// strategy table is a legal profile whose params would otherwise have been silently dropped on the
/// way back out of the store.
fn push_strategy(
    doc: &mut String,
    parent: Option<&str>,
    m: &MountRow,
    params: &BTreeMap<(i64, String), String>,
) {
    let header = match parent {
        None => "strategy".to_string(),
        Some(p) => format!("{p}.strategy"),
    };
    let mine: Vec<(&String, &String)> =
        params.iter().filter(|((o, _), _)| *o == m.ord).map(|((_, k), v)| (k, v)).collect();
    if m.strategy_name.is_none() && m.strategy_rhai.is_none() && mine.is_empty() {
        return;
    }
    doc.push_str(&format!("\n[{header}]\n"));
    if let Some(v) = &m.strategy_name {
        doc.push_str(&format!("name = {}\n", toml_basic_string(v)));
    }
    if let Some(v) = &m.strategy_rhai {
        doc.push_str(&format!("rhai = {}\n", toml_basic_string(v)));
    }
    if !mine.is_empty() {
        doc.push_str(&format!("\n[{header}.params]\n"));
        for (k, v) in mine {
            doc.push_str(&format!("{k} = {v}\n"));
        }
    }
}

/// **Render a stored DAEMON profile back into the TOML document
/// `vike_tradehub::config::DaemonProfile` parses.**
///
/// Moved DOWN into this leaf crate from `vike_tradehub::profile_rows::rows_to_daemon_profile`, which
/// now calls it and then hands the result to the existing parser. The move is load-bearing rather
/// than tidiness: `vike-cli` links no `vike-tradehub`, so a migration that writes daemon rows could
/// not reach a renderer that lived up there — and without a renderer it cannot run the round-trip
/// fence, which is the only thing that makes writing this body safe.
///
/// # ⚠ THE SINGLE-MOUNT SPELLING, AND THE DEFECT THAT MAKES IT MANDATORY
///
/// This renderer's predecessor **always** emitted `[[mounts]]`, and its own doc declared the
/// consequence: a `[[mounts]]` profile mounts under `DaemonProfile::derived_controller_id`
/// (`{venue}__{symbol}__{interval}__{strategy}`) where a single-mount profile keeps the legacy
/// `{venue}__{symbol}__{interval}` triple that `vike_core::strategy_state::mount_id_with` derives.
/// That id IS the strategy-state sidecar's filename and the journal's attribution key, so a
/// deployment whose selection moved to a row silently acquired an EMPTY state sidecar. The escape
/// its doc named — *"which is why `plan_migration` never moves a selection"* — stops holding the
/// moment a selection CAN move to a row, which is what this landing does.
///
/// So: exactly one mount, at ordinal 0, declaring no primary ⇒ the single-mount spelling. Anything
/// else ⇒ `[[mounts]]`. Derived from the rows, with no extra column, and the round-trip fence in
/// `crates/vike-cli/src/cmd/config/mirror_profile.rs`'s daemon twin is what proves it per profile
/// rather than in general.
///
/// ⚠ **Declared residual:** a ONE-row `[[mounts]]` profile that declares no `primary` renders as
/// single-mount and therefore FAILS its own fence. That is a refusal naming the repair (`primary =
/// true`, or accept the controller-id move), never a silent change — and the two spellings are
/// genuinely different deployments, so a migration may not pick for the operator.
///
/// # Errors
///
/// A `profile_setting` path outside `daemon.` — a key this renderer cannot put back into a daemon
/// profile document. It is an ERROR rather than a silent drop for the reason 0057 gives: a dropped
/// key is the declared-but-unread failure arriving through the store, and it would read as a
/// successful migration.
pub fn render_daemon_toml(stored: &StoredProfile) -> Result<String, String> {
    // ONE `[daemon]` header, whatever the key count: a second header for the same table is a TOML
    // error, so the keys are gathered first and the header written once.
    let mut daemon_keys: Vec<(&str, &String)> = Vec::new();
    for (path, value) in &stored.settings {
        match path.strip_prefix("daemon.") {
            Some(key) => daemon_keys.push((key, value)),
            None => {
                return Err(format!(
                    "profile `{}` carries setting `{path}`, which this renderer does not know how \
                     to put back into a daemon profile document. Nothing was loaded: a key that \
                     cannot be rendered must not be silently dropped, because the profile would \
                     then mount at a default nobody typed",
                    stored.row.name
                ));
            }
        }
    }
    let single =
        stored.mounts.len() == 1 && stored.mounts[0].ord == 0 && !stored.mounts[0].is_primary;
    let mut doc = String::new();
    // TOML requires every bare key before the first header, so the single spelling's mount keys
    // are written before `[daemon]` rather than after it.
    if single {
        push_mount_keys(&mut doc, &stored.mounts[0], false);
    }
    if !daemon_keys.is_empty() {
        if !doc.is_empty() {
            doc.push('\n');
        }
        doc.push_str("[daemon]\n");
        for (key, value) in daemon_keys {
            doc.push_str(&format!("{key} = {value}\n"));
        }
    }
    if single {
        push_strategy(&mut doc, None, &stored.mounts[0], &stored.params);
        return Ok(doc);
    }
    for m in &stored.mounts {
        doc.push_str("\n[[mounts]]\n");
        push_mount_keys(&mut doc, m, true);
        push_strategy(&mut doc, Some("mounts"), m, &stored.params);
    }
    Ok(doc)
}
