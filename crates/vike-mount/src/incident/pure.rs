//! Pure helpers of the incident collector: window selection, env redaction, hash and UTC stamps.

use vike_model::time::civil_from_days;

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

/// Does this env-var KEY name a secret whose value must be masked? Generous case-insensitive
/// substring match: over-redaction only hides a non-secret flag, a miss leaks a credential. Covers
/// `*_API_KEY` / `*_API_SECRET` / `*_API_PASSPHRASE` (`vike-bridge-core/src/credentials.rs`),
/// `*_PRIVATE_KEY` / `POLY_*_PK` (`bridges/polymarket/src/config.rs`), tokens, mnemonics, seeds,
/// signatures. `VIKE_*` runtime flags match no needle and pass verbatim.
pub fn is_secret_key(key: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PASSPHRASE",
        "TOKEN",
        "PRIVATE",
        "PRIVKEY",
        "MNEMONIC",
        "SEED",
        "SIGNATURE",
        "CREDENTIAL",
        "APIKEY",
        "API_KEY",
        "_KEY",
        "_PK",
        "WALLET",
    ];
    let up = key.to_ascii_uppercase();
    NEEDLES.iter().any(|&n| up.contains(n))
}

/// Mask a secret value EXACTLY as the signers' manual `Debug` impls do
/// (`crates/vike-bridge-core/src/signer.rs`): `***` plus the last four characters as a fingerprint;
/// under four chars, a bare `***`. Char-boundary-safe: never panics on a multibyte value (the
/// signers' byte-slice only ever sees ASCII keys).
pub fn redact_secret_value(val: &str) -> String {
    let n = val.chars().count();
    let tail: String = if n >= 4 { val.chars().skip(n - 4).collect() } else { String::new() };
    format!("***{tail}")
}

/// Sort an environment dump by key and mask every secret-shaped value ([`is_secret_key`] →
/// [`redact_secret_value`]). Pure over a slice, so tests never touch the process env.
pub fn redact_env(vars: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vars
        .iter()
        .map(|(k, v)| {
            if is_secret_key(k) {
                (k.clone(), redact_secret_value(v))
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// FNV-1a **64-bit** of `bytes` as 16 lowercase hex digits (constants as [`vike_exec::state_hash`],
/// inline: no hashing dependency). Pins the `RunProfile` TOML so a later edit is detectable.
pub fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Epoch-ms → UTC `(year, month, day, hour, minute, second, milli)`, chrono-free
/// ([`vike_model::time::civil_from_days`]).
fn utc_parts(ms: i64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let (y, mo, d) = civil_from_days(ms.div_euclid(86_400_000));
    let ms_of_day = ms.rem_euclid(86_400_000);
    let secs = ms_of_day / 1000;
    let millis = (ms_of_day % 1000) as u32;
    let h = (secs / 3600) as u32;
    let mi = ((secs % 3600) / 60) as u32;
    let s = (secs % 60) as u32;
    (y, mo, d, h, mi, s, millis)
}

/// Epoch-ms → `YYYYMMDDThhmmssZ`, the filesystem-safe bundle directory stamp.
pub fn utc_stamp_compact(ms: i64) -> String {
    let (y, mo, d, h, mi, s, _) = utc_parts(ms);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// Epoch-ms → `YYYY-MM-DDThh:mm:ss.sssZ`, for the manifest's human fields.
pub fn utc_stamp_rfc3339(ms: i64) -> String {
    let (y, mo, d, h, mi, s, millis) = utc_parts(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}
