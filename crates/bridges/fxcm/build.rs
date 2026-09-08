//! Compile the FXCM C ABI shim (`src/shim/fcshim.cpp`) and link ForexConnect —
//! ONLY under `--features fxcm`. Default/CI builds do nothing native here.
//!
//! Target-aware: builds against the Windows SDK (`ForexConnect.lib`) or the Linux x86_64 SDK
//! (`libForexConnect.so`); the shim source is one portable C++ file (POSIX-ported off `windows.h`).
//! macOS is DEFERRED — the `.dylib` set is staged but its Mach-O install_name/@rpath wiring is not
//! done, so the feature fails loudly there rather than silently mislinking.
//!
//! The proprietary SDK is not vendored into git. It is found via:
//!   1. `FCSDK_DIR` env var (honored verbatim — point it at the platform SDK root), else
//!   2. a platform default under `<workspace>/vendor/fcsdk` (gitignored):
//!        - windows: `vendor/fcsdk`
//!        - linux:   `vendor/fcsdk/linux`
//!
//! When the SDK is absent, the `fcsdk` cfg stays unset and `src/sys.rs`
//! compiles a stub whose calls return `FxcmError::Unavailable`.
//!
//! Moved into crates/bridges/fxcm (crate-reorg Phase 3, PR D): this crate now lives at
//! `crates/bridges/fxcm`, one level deeper than the old shared venue-adapter crate, so the
//! `vendor/fcsdk` workspace-root fallback gained a third `..`.
use std::path::{Path, PathBuf};

fn main() {
    // Declared unconditionally so `#[cfg(fcsdk)]` is never an "unexpected cfg".
    println!("cargo:rustc-check-cfg=cfg(fcsdk)");
    println!("cargo:rerun-if-env-changed=FCSDK_DIR");
    println!("cargo:rerun-if-changed=src/shim/fcshim.cpp");

    // Native C++ only when the fxcm feature is enabled.
    if std::env::var_os("CARGO_FEATURE_FXCM").is_none() {
        return;
    }

    // The TARGET's OS, not the host's — build scripts must read this env var, because the `cfg!`
    // macro in a build script reflects the HOST (a well-known footgun for cross builds).
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // ── macOS: PANIC, deliberately, and the argument is not the obvious one ──────────────────────
    //
    // Every other unsupported configuration here DEGRADES: `--features fxcm` on a Linux or Windows
    // box with no SDK prints a warning, returns, and compiles the `#[cfg(not(fcsdk))]` stub. So a
    // reader is right to ask why this one platform is a hard failure instead, and "a half-wired
    // .dylib set that mislinks is worse than a refusal" is only half the answer — it argues against
    // LINKING, not against degrading to the same stub as everywhere else.
    //
    // The whole answer is that on macOS the stub is not a DEGRADATION, it is the only outcome that
    // will ever exist. Those other two are "the SDK is absent", a recoverable and temporary state
    // the `fcsdk` cfg handles end to end: stage the SDK, rebuild, and the same command links. macOS
    // is "this platform is not wired", which no amount of staging changes — the `.dylib` set is
    // already sitting under `vendor/fcsdk/macos` and its Mach-O install_name/@rpath wiring is what
    // is missing. Degrading would silently promise that staging the SDK is the fix, on the one
    // platform where it is not.
    //
    // And the cost of finding out late is real rather than theoretical: a stub build is a perfectly
    // ordinary binary. `vike_fxcm::sdk_linked()` returns false, so `vike_mount::make_engine`'s
    // `("fxcm", _)` arm refuses the live mount and lands on paper with an `error!` — at RUNTIME, on
    // a box with credentials, after a daemon has started. The panic moves that discovery to the
    // earliest moment the answer is available, which is compile time, because on this target the
    // answer cannot change.
    //
    // ⚠ Not gated, and that is a statement rather than an omission: this workspace has no macOS in
    // CI (every runner is self-hosted Linux) and no macOS dev box, so nothing here can execute this
    // branch. It is reviewed, not measured. If a mac ever joins, the honest gate is a build of this
    // crate under `--features fxcm` on it, asserting the message — not a test of `target_os`
    // string-matching, which would assert the code says what it says.
    if target_os == "macos" {
        panic!(
            "vike-fxcm: the `fxcm` feature is not supported on macOS yet. The ForexConnect .dylib \
             set is staged under vendor/fcsdk/macos, but its Mach-O install_name/@rpath wiring is \
             deferred. Build for Windows or Linux (x86_64), or drop the `fxcm` feature for the \
             cross-platform stub (which builds on macOS fine)."
        );
    }
    let linux = target_os == "linux";

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Per-platform SDK root default (FCSDK_DIR always wins, honored verbatim). On Linux the vendored
    // default gains a `linux/` subdir; Windows keeps the flat `vendor/fcsdk` layout it always used.
    let sdk: PathBuf = std::env::var("FCSDK_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        // crates/bridges/fxcm -> ../../../vendor/fcsdk (workspace root); one `..` deeper than
        // the old shared venue-adapter crate -> ../../vendor/fcsdk (crate-reorg Phase 3, PR D).
        let root =
            Path::new(&manifest).join("..").join("..").join("..").join("vendor").join("fcsdk");
        if linux { root.join("linux") } else { root }
    });

    let header = sdk.join("include").join("forexconnect").join("ForexConnect.h");
    // Platform link target: libForexConnect.so (Linux) vs ForexConnect.lib (Windows).
    let lib = if linux {
        sdk.join("lib").join("libForexConnect.so")
    } else {
        sdk.join("lib").join("ForexConnect.lib")
    };

    // ⚠ The two paths the gate below STATS must also be paths cargo WATCHES, or the stub-vs-linked
    // decision is cached across the very change that should flip it. Until this existed the script
    // declared only `FCSDK_DIR` and the shim source, so STAGING or REMOVING the SDK left the
    // previous verdict in place.
    //
    // Measured 2026-08-25 on the CI box lane `vike-fresh3`: a `vendor/fcsdk` symlink was removed and the
    // lane kept the cached LINKED verdict, so every branch checked out there failed to link with
    // `unable to find library -lForexConnect` — a red that belonged to no branch. `cargo clean -p
    // vike-fxcm` cleared it, which is the tell that the input was untracked rather than wrong.
    //
    // A declared path that does NOT exist is not an error: cargo treats it as changed, so the
    // script re-runs and re-decides the moment the SDK appears. That costs one extra script run per
    // build on an SDK-less box WITH the feature on — two `exists()` calls and a `println!` — and
    // buys back the case where the answer silently stops matching the disk. It cannot fire on a
    // default build at all: the `CARGO_FEATURE_FXCM` return above is upstream of this line.
    println!("cargo:rerun-if-changed={}", header.display());
    println!("cargo:rerun-if-changed={}", lib.display());

    // SDK-present gate (Windows + Linux): no SDK -> stub build, so a box without the SDK still
    // compiles. Preserved from the original build.rs.
    if !(header.exists() && lib.exists()) {
        println!(
            "cargo:warning=vike-fxcm: ForexConnect SDK not found at {} — building STUB (set FCSDK_DIR to enable FXCM).",
            sdk.display()
        );
        return;
    }

    let sdk_lib = sdk.join("lib");
    let mut build = cc::Build::new();
    build.cpp(true).file("src/shim/fcshim.cpp").include(sdk.join("include"));

    if linux {
        // THE load-bearing Linux fact: the staged SDK was built with the pre-gcc5 libstdc++ string
        // ABI (measured `__cxx11`=0 across every lib, old-ABI `basic_string` symbols present). A
        // modern GCC defaults to `_GLIBCXX_USE_CXX11_ABI=1`, which mangles `std::string` the new
        // way — any std::string crossing the ForexConnect boundary then fails to link (undefined
        // reference to the old-ABI symbol). Compile the shim old-ABI to match.
        build.define("_GLIBCXX_USE_CXX11_ABI", Some("0"));
        build.flag_if_supported("-std=c++11");
    }
    build.compile("fcshim"); // cc links libstdc++ automatically for a .cpp(true) build.

    println!("cargo:rustc-link-search=native={}", sdk_lib.display());
    println!("cargo:rustc-link-lib=ForexConnect"); // resolves ForexConnect.lib / libForexConnect.so
    println!("cargo:rustc-cfg=fcsdk");

    if linux {
        // ── THE BAKED RPATH: exactly one entry, `$ORIGIN/../lib`, and NOTHING ABSOLUTE ───────────
        //
        // The SDK's own DT_RPATH is `.` plus dead Jenkins build paths, so the loader cannot find the
        // sibling .so's from an arbitrary CWD. This bakes our own — RELOCATABLE ONLY. The step that
        // populates that directory for an installed project is
        // `crates/bridges/fxcm/scripts/package-fcsdk-runtime.sh` (`just fxcm-package <root>`), which
        // copies the set `crates/bridges/fxcm/FCSDK.linux.sha256` pins — SONAME-versioned twins
        // included, which a copy of the plain `libX.so` names alone would miss — into `<root>/lib`.
        //
        // ⚠ **AN ABSOLUTE `-Wl,-rpath,<sdk>/lib` USED TO BE EMITTED HERE, FIRST, AND IT IS GONE.**
        // `DT_RPATH` is one colon-separated string searched LEFT TO RIGHT, so that entry — the path
        // of whatever SDK tree THIS build happened to be pointed at — was the first directory the
        // loader tried. `scripts/release_fxcm_artifact.sh` restated this whole list on the shipped
        // artifact, so the published `vike-tradehub-fxcm` resolved through
        // `/var/lib/vike/fcsdk/linux/lib` on the CI box: a tree beside the CI lanes that agents
        // create and delete, never packaged, never deployed, guaranteed by nothing. On the CI box the
        // release runner IS the deploy box, which is exactly why nothing caught it — it linked,
        // `ldd` was clean, `--version` ran, and the `<root>/lib` an operator packaged was never
        // read. It was not a binary that worked; it was one that had not failed yet.
        //
        // The first fix was to subtract it in the release script and keep it here "for dev". That
        // fenced the class instead of removing it, and it does not survive the third deployment:
        //
        //   1. **from a checkout** — the test binaries this script's args actually reach. `$ORIGIN`
        //      is `target/<profile>/deps`, and no relative path from there to the vendor tree is
        //      expressible (`CARGO_TARGET_DIR` can be anywhere — measured, on a run that put it
        //      under `/var/lib/vike/`). So this case is served by `LD_LIBRARY_PATH`, which
        //      `crates/bridges/fxcm/scripts/provision-fcsdk.sh` prints and
        //      `docs/ops/fxcm-forexconnect-the CI box.md` Step 3 already exports for the smokes.
        //   2. **an installed project** — `<root>/bin/<exe>` with `<root>/lib` filled by the
        //      packaging step. This is what `$ORIGIN/../lib` is for.
        //   3. **a container image with the SDK baked in** — now a supported shape. The libraries
        //      sit at a path the Dockerfile chose, which is neither the build box's nor
        //      `$ORIGIN/../lib` when the project folder is a MOUNT. An absolute entry pointing at
        //      the build box would be searched first and answer nothing; the image supplies the
        //      path instead, with `ENV LD_LIBRARY_PATH=<sdk>/lib` or an `/etc/ld.so.conf.d` entry
        //      plus `ldconfig`. Both work here precisely BECAUSE the baked rpath is only
        //      `$ORIGIN/../lib`: a non-existent rpath directory is skipped and the loader falls
        //      through — whereas `DT_RPATH` outranks `LD_LIBRARY_PATH`, so a stale absolute entry
        //      that DID exist would win over the image's own configuration.
        //
        // One entry serves all three, so there is nothing left to subtract downstream and no build
        // in this tree bakes a machine-specific path. `scripts/release_fxcm_artifact.sh` passes the
        // SAME list (it must restate it — see below), pinned equal by
        // `crates/vike-ops/tests/release_fxcm_artifact_gate.rs`, and
        // `crates/bridges/fxcm/tests/fcsdk_rpath_tag.rs` LINKS a probe with each list and reads the
        // entries off the produced ELF.
        //
        // ⚠ ...and these reach THIS PACKAGE'S OWN TARGETS AND NOTHING ELSE. Cargo scopes
        // `cargo:rustc-link-arg` to the targets of the package whose build script emitted it,
        // while `rustc-link-lib`/`rustc-link-search` above ARE link metadata and DO propagate to
        // dependents. So a downstream binary that turns the feature on inherits the
        // `-lForexConnect` and inherits none of the rpath. MEASURED the first time anything
        // downstream was ever linked (the `vike-tradehub-fxcm` release artifact): `NEEDED
        // libForexConnect.so` present, rpath tag ABSENT, and the run refused by
        // `scripts/release_fxcm_artifact.sh`'s `verify`. Nothing was wrong here — the two lines
        // below are correct and `crates/bridges/fxcm/tests/fcsdk_rpath_tag.rs` proves it — the
        // reach is simply the package. A consumer that links this crate into a SHIPPED binary must
        // set the same args itself on the final build, and that script is the one place that does
        // (`-C link-arg=` via RUSTFLAGS, pinned against these lines by
        // `crates/vike-ops/tests/release_fxcm_artifact_gate.rs`).
        //
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
        // ⚠ …and the rpath above is INERT without this line, which is why it is here and why it
        // must not be tidied away for looking deprecated.
        //
        // A modern `ld` defaults to "new dtags" and records `-rpath` as **`DT_RUNPATH`**, not
        // `DT_RPATH`. Per `ld.so(8)` the two are not interchangeable: `DT_RUNPATH` is consulted
        // ONLY for the object's own direct `DT_NEEDED` entries and is NOT inherited by their
        // dependencies, while `DT_RPATH` applies transitively down the whole chain. This binary's
        // only direct NEEDED on the SDK is `libForexConnect.so`; its ~20 siblings
        // (`libgsexpat.so`, `liblog4cplus.so.4`, …) are that library's dependencies, i.e.
        // GRANDCHILDREN of the executable — so under `DT_RUNPATH` they are searched for as if no
        // rpath had been baked at all.
        //
        // MEASURED on the CI box against a real `--features fxcm` binary and a correctly packaged
        // `lib/`, with the absolute vendor-tree rpath entry hidden so only `$ORIGIN/../lib` could
        // answer:
        //     DT_RUNPATH (before this line):  ldd → 5 × "not found";  run → exit 127,
        //                                     "libgsexpat.so: cannot open shared object file"
        //     DT_RPATH   (with this line):    ldd → 0 × "not found";  run → exit 0
        // So the relocatable install path had never worked — not because the directory was empty
        // (the packaging step fills it) but because the loader never read it. `-Wl,-rpath` alone
        // is a spelling that LOOKS right and is checkable only by reading the emitted TAG, which
        // is what `crates/bridges/fxcm/tests/fcsdk_rpath_tag.rs` does.
        //
        // ⚠ That test is deliberately NOT a grep for `--disable-new-dtags`, and this paragraph is
        // why such a grep would be worthless: the argument for the flag has to NAME the flag, so
        // the source keeps saying `--disable-new-dtags` whether or not anything still emits it. The
        // test links a probe with the args this file actually emits and reads the tag off the
        // result — deleting the line below while leaving every word of this comment in place turns
        // it red, which is the property that was checked by planting exactly that.
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
        // Linux twin of the Windows FCSDK_BIN: where the runtime .so's live, for a packaging step.
        println!("cargo:rustc-env=FCSDK_LIB={}", sdk_lib.display());
    } else {
        // Windows: expose the SDK bin dir so a dependent binary can copy the runtime DLLs next to
        // its exe (unchanged from the original build.rs).
        println!("cargo:rustc-env=FCSDK_BIN={}", sdk.join("bin").display());
    }
}
