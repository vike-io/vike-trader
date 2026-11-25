//! Pure helpers of the incident collector: window selection, env redaction and UTC stamps.

// ---- pure helpers (unit-tested) ----

/// Window-overlap selection: of `(label, mtime epoch-ms)` pairs, the labels whose mtime is AT OR
/// AFTER the cutoff `now_ms - since_ms` (inclusive; saturating, so a huge window cannot overflow).
///
/// Sound because the stores are append-only and time-ordered: a file whose LAST write precedes the
/// cutoff holds no record in the window, and one straddling it is kept (extra context is fine).
/// Generic: selects journal segments AND trace-log files.
pub fn select_overlapping<T: Clone>(items: &[(T, i64)], now_ms: i64, since_ms: i64) -> Vec<T> {
    let cutoff = now_ms.saturating_sub(since_ms);
    items.iter().filter(|(_, mtime)| *mtime >= cutoff).map(|(t, _)| t.clone()).collect()
}

/// Sort an environment dump by key and mask every secret-shaped value
/// ([`vike_config::redact::matches_secret_rule`]) EXACTLY as the signers' manual `Debug` impls do: `***`
/// plus [`vike_model::secret::key_tail`], the last four characters as a fingerprint (a bare `***` under
/// four). Which names are
/// secret-shaped is `vike-config`'s one rule table, shared with `config show` and the MCP
/// transcript: this module spells no list of its own. It calls the STRICT predicate, not
/// `is_secret`: that one excuses a few flag/count/path names so `config show` stays readable, but a
/// bundle is pasted into issues and over-masking a harmless knob costs nothing here. Pure over a
/// slice, so tests never touch the process env.
pub fn redact_env(vars: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vars
        .iter()
        .map(|(k, v)| {
            if vike_config::redact::matches_secret_rule(k) {
                (k.clone(), format!("***{}", vike_model::secret::key_tail(v)))
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Epoch-ms → `YYYYMMDDThhmmssZ`, the filesystem-safe bundle directory stamp. (The manifest's human
/// fields take the dashed millisecond form, `vike_model::time::epoch_ms_to_utc_timestamp_ms`.)
pub fn utc_stamp_compact(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = vike_model::time::utc_parts(ms);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}
