//! Embeds the Vike icon into `vike-desktop.exe` (design system spec §6).
//!
//! Explorer, the Start menu, a shortcut and a pinned taskbar button read an executable's icon from
//! its RESOURCE section. The running window's icon is a different one, set by eframe from
//! `vike_ui_theme::brand::window_icon`. This is the committed, drift-gated
//! `assets/brand/vike-desktop.ico`.
//!
//! Only `x86_64-pc-windows-gnu` gets it: every shipped `.exe` is cross-built for that target
//! (`.github/workflows/release.yml`'s `windows` job). Its resource compiler,
//! `x86_64-w64-mingw32-windres`, ships in `binutils-mingw-w64-x86-64`, which the mingw gcc that job
//! and the `windows-cross` lane already need depends on. A windres that is missing or fails FAILS
//! the build: an `.exe` without its icon is a regression nobody would notice. An MSVC build warns
//! and goes without; every other target is untouched.
//!
//! windres runs INSIDE `OUT_DIR` on relative names. MEASURED 2026-09-29 (binutils 2.41.90): it
//! writes no path into its object with either spelling. The relative one keeps that true whatever
//! a later windres does, because a runner's absolute path names the box, and
//! `scripts/refuse_box_paths.sh` refuses such a release asset at tag time.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let ico = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand/vike-desktop.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-changed=build.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.ends_with("-windows-msvc") {
        println!(
            "cargo:warning=vike-desktop: an MSVC build carries no icon resource; the shipped .exe \
             is cross-built for x86_64-pc-windows-gnu, which embeds assets/brand/vike-desktop.ico"
        );
        return;
    }
    if target != "x86_64-pc-windows-gnu" {
        return;
    }
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR for a build script"));
    std::fs::copy(&ico, out.join("vike-desktop.ico")).expect("copy the icon into OUT_DIR");
    std::fs::write(out.join("vike-desktop.rc"), "1 ICON \"vike-desktop.ico\"\n")
        .expect("write the resource script into OUT_DIR");
    let status = Command::new("x86_64-w64-mingw32-windres")
        .current_dir(&out)
        .args(["--input", "vike-desktop.rc", "--output", "vike-desktop-res.o"])
        .args(["--output-format", "coff"])
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "x86_64-w64-mingw32-windres did not start ({e}); it ships in \
                 binutils-mingw-w64-x86-64, a dependency of gcc-mingw-w64-x86-64"
            )
        });
    assert!(status.success(), "x86_64-w64-mingw32-windres failed on vike-desktop.rc: {status}");
    println!("cargo:rustc-link-arg-bin=vike-desktop={}", out.join("vike-desktop-res.o").display());
}
