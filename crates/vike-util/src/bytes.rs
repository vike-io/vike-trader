//! The ONE byte-count display string, in IEC units (decision 0141, amended 2026-10-13). It used to
//! be three hand-written flavours (a base-1024 one that printed `KB`/`MB`, a base-1000 one that took
//! a fractional estimate, and a `MiB`/`GiB`-only one for the disk preflight), and the same 1 000 000
//! bytes read `976.6 KB`, `1.0 MB` or `1000000 B` depending on the screen. A size an operator reads
//! is now [`iec_bytes`] everywhere: the GUI data screens, the `data hist import` plan's
//! store-growth line and the startup preflight's disk check.
//!
//! | function | divides by | prints | input |
//! |---|---|---|---|
//! | [`iec_bytes`] | 1024 | `B` `KiB` `MiB` `GiB` `TiB` | whole bytes (`u64`) |
//!
//! There is no SI (base-1000) flavour: no caller compared a size against a decimal figure, so a
//! second function would only be a second text for one number. A caller holding a FRACTIONAL
//! estimate (a measured bytes-per-row times a row count) rounds to whole bytes AT ITS OWN SITE
//! (`vike-cli`'s `data hist import` plan does), because rounding is the caller's statement about
//! its estimate, not this function's. A future caller that truly speaks SI units (a venue or
//! network quantity quoted in `kB`/`MB`) is the reopener: it gets its own named function here, not
//! a flag on this one.

/// Whole bytes, scaled by 1024 and printed with the IEC labels: `B`, `KiB`, `MiB`, `GiB`, `TiB`,
/// one decimal place above `B`, `0` as `"0 B"`, and no unit past `TiB` (a 2000 TiB total prints
/// `"2000.0 TiB"`). `1536` is `"1.5 KiB"`; `1023` is `"1023 B"` and `1024` is `"1.0 KiB"`.
pub fn iec_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut val = bytes as f64;
    let mut unit = 0usize;
    while val >= 1024.0 && unit < UNITS.len() - 1 {
        val /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} {}", UNITS[unit]) } else { format!("{val:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
#[path = "bytes_tests.rs"]
mod tests;
