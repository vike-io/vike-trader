//! Golden outputs of [`super::iec_bytes`]. The scale, the one decimal and the `0 B` / `NNN B`
//! shapes are what the three hand-written flavours it replaced printed (`vike_ui_theme::fmt::fmt_bytes`
//! and `vike-data-manager`'s copy of it, `vike-cli`'s `human_bytes`, `vike-mount`'s preflight
//! `fmt_bytes`); only the base and the labels changed, so these goldens are those functions' old
//! ones with the text moved to IEC.

use super::iec_bytes;

const KIB: u64 = 1024;

#[test]
fn iec_bytes_scales_by_1024_with_iec_labels() {
    assert_eq!(iec_bytes(0), "0 B");
    assert_eq!(iec_bytes(1), "1 B");
    assert_eq!(iec_bytes(512), "512 B");
    assert_eq!(iec_bytes(1536), "1.5 KiB");
    assert_eq!(iec_bytes(1_572_864), "1.5 MiB"); // 1.5 * 1024 * 1024
    assert_eq!(iec_bytes(KIB.pow(3) * 2), "2.0 GiB");
    assert_eq!(iec_bytes(KIB.pow(4) * 3), "3.0 TiB");
    assert_eq!(iec_bytes(5 * KIB.pow(3)), "5.0 GiB"); // vike-mount's default disk warn floor
}

#[test]
fn iec_bytes_edges_are_the_unit_boundaries_and_the_tib_cap() {
    assert_eq!(iec_bytes(1023), "1023 B");
    assert_eq!(iec_bytes(1024), "1.0 KiB");
    // The 1 MiB boundary: one byte short stays in KiB, the boundary itself is MiB.
    assert_eq!(iec_bytes(KIB * KIB - 1), "1024.0 KiB");
    assert_eq!(iec_bytes(KIB * KIB), "1.0 MiB");
    // No unit past TiB: a bigger total keeps counting TiB.
    assert_eq!(iec_bytes(KIB.pow(4) * 2000), "2000.0 TiB");
}

/// The disk preflight's floors are compared in bytes and only PRINTED through this; the sizes the
/// check used to print as `2048 B` and `3072.0 GiB` now have a `KiB` and a `TiB` tier.
#[test]
fn iec_bytes_has_a_kib_and_a_tib_tier_the_disk_check_used_to_lack() {
    assert_eq!(iec_bytes(2048), "2.0 KiB");
    assert_eq!(iec_bytes(KIB.pow(4) * 3), "3.0 TiB");
}

/// History, pinned so it cannot come back unnoticed: the same 1 000 000 bytes read `976.6 KB`,
/// `1.0 MB` and `1000000 B` from the three flavours this one replaced; it reads one thing.
#[test]
fn one_million_bytes_reads_one_way() {
    assert_eq!(iec_bytes(1_000_000), "976.6 KiB");
}
