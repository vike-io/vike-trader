//! Build the FXCM C ABI shim (`src/shim/fcshim.cpp`) as a SHARED OBJECT that links ForexConnect —
//! ONLY under `--features fxcm`. Default/CI builds do nothing native here.
//!
//! ⚠ **This script used to link the SDK INTO the Rust binary, and stopped on 2026-09-09.** It
//! compiled the shim into a static archive and emitted `cargo:rustc-link-lib=ForexConnect`, so
//! every dependent binary carried a hard `DT_NEEDED libForexConnect.so` and did not reach `main` on
//! a box without the library set staged — exit 127, before a line of ours ran. It now produces
//! `libfcshim.so` (or `fcshim.dll`), which links the SDK the ordinary way, and
//! `crates/bridges/fxcm/src/loader.rs` opens THAT at runtime. There is no `fcsdk` cfg any more and
//! no stub: the Rust code is identical on every box, and the SDK's presence is a runtime fact.
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
//! When the SDK is absent this script builds nothing and returns. Nothing about the Rust build
//! changes; the shim simply is not there for the loader to open, and every call answers
//! `FxcmError::Unavailable` — the same answer the old compile-time stub gave.
//!
//! Moved into crates/bridges/fxcm (crate-reorg Phase 3, PR D): this crate now lives at
//! `crates/bridges/fxcm`, one level deeper than the old shared venue-adapter crate, so the
//! `vendor/fcsdk` workspace-root fallback gained a third `..`.
use std::path::{Path, PathBuf};

fn main() {
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
    // box with no SDK prints a warning, returns, and builds no shim — so the loader finds nothing
    // and every call answers `Unavailable`. So a reader is right to ask why this one platform is a
    // hard failure instead, and "a half-wired .dylib set that mislinks is worse than a refusal" is
    // only half the answer — it argues against LINKING, not against degrading as everywhere else.
    //
    // The whole answer is that on macOS "no shim" is not a DEGRADATION, it is the only outcome that
    // will ever exist. Those other two are "the SDK is absent", a recoverable and temporary state
    // the shim handles end to end: stage the SDK, rebuild, and the same command produces one. macOS
    // is "this platform is not wired", which no amount of staging changes — the `.dylib` set is
    // already sitting under `vendor/fcsdk/macos` and its Mach-O install_name/@rpath wiring is what
    // is missing. Degrading would silently promise that staging the SDK is the fix, on the one
    // platform where it is not.
    //
    // And the cost of finding out late is real rather than theoretical: a shim-less build is a
    // perfectly ordinary binary. `vike_fxcm::sdk_available()` returns false, so `vike_mount::make_engine`'s
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
             cross-platform build (which compiles on macOS fine and leaves FXCM on paper)."
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

    // ⚠ The two paths the gate below STATS must also be paths cargo WATCHES, or the build-a-shim
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

    // SDK-present gate (Windows + Linux): no SDK -> no shim, so a box without the SDK still
    // compiles. Preserved from the original build.rs.
    if !(header.exists() && lib.exists()) {
        println!(
            "cargo:warning=vike-fxcm: ForexConnect SDK not found at {} — building NO SHIM, so FXCM stays paper at runtime (set FCSDK_DIR to enable it).",
            sdk.display()
        );
        return;
    }

    let sdk_lib = sdk.join("lib");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let shim = out_dir.join(if linux { "libfcshim.so" } else { "fcshim.dll" });

    // ── THE SHIM IS ITS OWN SHARED OBJECT, AND USED TO BE A STATIC ARCHIVE ───────────────────────
    //
    // Until 2026-09-09 this compiled `fcshim.cpp` with `cc::Build::compile("fcshim")` — a static
    // archive linked straight into every dependent Rust binary — and then emitted
    // `cargo:rustc-link-lib=ForexConnect`. That made the binary carry a hard `DT_NEEDED
    // libForexConnect.so`: on a box without the library set staged it did not reach `main`, exit
    // 127, before a line of ours ran. So the feature could never be part of a universal build, and
    // the release carried a SECOND daemon asset for the boxes that trade FX.
    //
    // Now the C++ boundary is a shared object that links the SDK the ordinary way, and
    // `crates/bridges/fxcm/src/loader.rs` opens it at runtime. Nothing Rust-side references a
    // ForexConnect symbol, so no `rustc-link-lib`, no `rustc-link-search`, and no `fcsdk` cfg —
    // the Rust code is identical on every box and the SDK's presence is a runtime fact.
    //
    // ⚠ THE OBVIOUS SMALLER CHANGE — `dlopen` the SDK itself and `dlsym` `CO2GTransport::
    // createSession` — WAS PLANNED AND IS WRONG. Compiling this shim against the real SDK and
    // reading `nm -u -C` gives FIVE undefined SDK symbols, not one: the two listener base classes'
    // out-of-line constructors, `IAddRef`'s out-of-line virtual destructor and its typeinfo come
    // with it, because the shim DERIVES from those interfaces. `loader.rs`'s header carries the
    // measurement and what defining them ourselves would cost.
    let compiler = cc::Build::new().cpp(true).get_compiler();
    let mut cmd = std::process::Command::new(compiler.path());
    cmd.arg("src/shim/fcshim.cpp").arg("-I").arg(sdk.join("include"));

    if linux {
        // THE load-bearing Linux fact: the staged SDK was built with the pre-gcc5 libstdc++ string
        // ABI (measured `__cxx11`=0 across every lib, old-ABI `basic_string` symbols present). A
        // modern GCC defaults to `_GLIBCXX_USE_CXX11_ABI=1`, which mangles `std::string` the new
        // way — any std::string crossing the ForexConnect boundary then fails to link (undefined
        // reference to the old-ABI symbol). Compile the shim old-ABI to match.
        cmd.args(["-D_GLIBCXX_USE_CXX11_ABI=0", "-std=c++11", "-shared", "-fPIC", "-O2"]);
        cmd.arg("-o").arg(&shim);
        cmd.arg(format!("-L{}", sdk_lib.display())).arg("-lForexConnect");
        // ── THE SHIM'S OWN RPATH: exactly one entry, `$ORIGIN`, and NOTHING ABSOLUTE ─────────────
        //
        // This is where the rpath argument MOVED to, and it is a better home than the one it left.
        // It used to be emitted as `cargo:rustc-link-arg` for THIS PACKAGE'S targets only, so a
        // downstream binary inherited none of it and `scripts/release_fxcm_artifact.sh` had to
        // restate the whole list through RUSTFLAGS — a second spelling held equal by a gate. The
        // shim carries its own now: wherever the file is installed, its ~20 sibling libraries are
        // found beside it, and no consumer needs to know that.
        //
        // ⚠ `--disable-new-dtags` is LOAD-BEARING and must not be tidied away for looking
        // deprecated. A modern `ld` records `-rpath` as `DT_RUNPATH`, which per `ld.so(8)` is
        // consulted ONLY for the object's own direct `DT_NEEDED` entries and is NOT inherited by
        // their dependencies. This object's only direct NEEDED on the SDK is `libForexConnect.so`;
        // its ~20 siblings (`libgsexpat.so`, `liblog4cplus.so.4`, …) are that library's
        // dependencies, i.e. grandchildren — so under `DT_RUNPATH` they are searched for as if no
        // rpath had been baked at all. MEASURED on the CI box against the real SDK before this change
        // was written, on the executable that used to carry these flags:
        //     DT_RUNPATH:  ldd → 5 × "not found";  run → exit 127, "libgsexpat.so: cannot open …"
        //     DT_RPATH:    ldd → 0 × "not found";  run → exit 0
        // and re-measured on the SHIM itself afterwards: `readelf -d` reports RPATH, and `ldd` from
        // an unrelated working directory reports zero missing libraries.
        //
        // ⚠ NOTHING ABSOLUTE. An absolute `-rpath <sdk>/lib` used to be emitted FIRST, and a
        // published binary therefore resolved through the release runner's own vendor tree — a
        // directory beside the CI lanes that agents create and delete. It was not a binary that
        // worked; it was one that had not failed yet. `DT_RPATH` outranks `LD_LIBRARY_PATH`, so a
        // stale absolute entry that DID exist would win over an image's own configuration.
        cmd.args(["-Wl,-rpath,$ORIGIN", "-Wl,--disable-new-dtags"]);
    } else {
        // ⚠ WINDOWS IS REVIEWED, NOT MEASURED, and this comment is the whole of the warranty. No
        // runner and no dev box in this workspace has ever built this crate with `--features fxcm`
        // for a Windows target — every CI lane is Linux and the `windows-cross` lane does not name
        // this crate — so what follows is the MSVC spelling of the branch above and nothing has
        // executed it. A `.dll` built here exports the same ten `fc_*` symbols and
        // `crates/bridges/fxcm/src/loader.rs` resolves them with `GetProcAddress`; what is unproven
        // is this command line, not the design. Windows has no `$ORIGIN`, and it needs none: the
        // loader searches the directory of the module being loaded before anything else, so a
        // `fcshim.dll` beside the SDK's own DLLs finds them.
        cmd.args(["/LD", "/EHsc", "/O2"]);
        cmd.arg(format!("/Fe:{}", shim.display()));
        cmd.arg("/link").arg(format!("/LIBPATH:{}", sdk_lib.display())).arg("ForexConnect.lib");
    }

    let status = cmd.status().unwrap_or_else(|e| panic!("vike-fxcm: cannot run {cmd:?}: {e}"));
    assert!(status.success(), "vike-fxcm: building the shim failed: {cmd:?} exited {status}");

    // ── THE DEV RUNGS: put the shim where a locally-built binary will look ───────────────────────
    //
    // `loader.rs`'s ladder is `<exe_dir>/../lib/`, then `<exe_dir>/`, then the loader's own search.
    // For an INSTALLED project the first answers and the packaging step fills it. For a binary run
    // out of a checkout, `<exe_dir>` is the cargo profile directory (`target/release/backtest`) or
    // its `deps/` sibling (a test binary), so the shim is copied to both — otherwise every local
    // run would need the shim's directory on `LD_LIBRARY_PATH`, which is a step a developer forgets
    // once and then debugs as "FXCM is unavailable".
    //
    // ⚠ The profile directory is derived from `OUT_DIR` because cargo declares no variable for it:
    // `OUT_DIR` is `<target>/<profile>/build/<pkg>-<hash>/out`, so three pops reach it. This is a
    // documented-but-unstable layout rather than a contract, so a failure to copy is a WARNING and
    // not a build failure — the installed and container shapes do not depend on it at all, and a
    // developer who hits the gap has `LD_LIBRARY_PATH`.
    if let Some(profile_dir) = out_dir.ancestors().nth(3) {
        for dest_dir in [profile_dir.to_path_buf(), profile_dir.join("deps")] {
            let dest = dest_dir.join(shim.file_name().expect("the shim has a file name"));
            if std::fs::create_dir_all(&dest_dir).is_err() || std::fs::copy(&shim, &dest).is_err() {
                println!(
                    "cargo:warning=vike-fxcm: could not stage the shim at {} — a locally-built \
                     binary will need its directory on the loader's search path",
                    dest.display()
                );
            }
        }
    }
    // ⚠ NO `cargo:rustc-env` NAMING THE BUILT PATH, deliberately. It is the obvious way to let the
    // Rust side or a packaging step find the file, and it bakes the BUILD BOX's directory into the
    // binary as an `env!` literal — which `scripts/refuse_box_paths.sh` refuses on every release
    // asset, at TAG time, which is the worst moment to find out. `loader.rs` resolves at runtime and
    // reads no build-time path at all; a packaging step takes the copy this script just staged in
    // the cargo profile directory.

    // ── WHAT THIS SCRIPT NO LONGER EMITS, AND WHY THE ABSENCE IS THE POINT ───────────────────────
    //
    // A ~100-line block stood here until 2026-09-09 emitting, for Linux:
    //
    //     cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib
    //     cargo:rustc-link-arg=-Wl,--disable-new-dtags
    //     cargo:rustc-env=FCSDK_LIB=<sdk>/lib          (and the Windows FCSDK_BIN twin)
    //
    // Every one of them is gone, and each for its own reason:
    //
    //   * the two rpath args were for THIS PACKAGE'S targets, and cargo scopes `rustc-link-arg` to
    //     exactly that — so a downstream binary that turned the feature on inherited the
    //     `-lForexConnect` and NONE of the rpath. MEASURED the first time anything downstream was
    //     ever linked (the `vike-tradehub-fxcm` release artifact): `NEEDED libForexConnect.so`
    //     present, rpath tag ABSENT. The fix at the time was for
    //     `scripts/release_fxcm_artifact.sh` to restate the whole list through RUSTFLAGS, held
    //     equal to these lines by a gate — a second spelling of one fact, which is the shape this
    //     tree spends most of its gates on. The shim carries its own `$ORIGIN` rpath now (see
    //     above), so there is no list to restate and no consumer that needs one.
    //   * `FCSDK_LIB`/`FCSDK_BIN` were read by NOTHING. `git grep` finds them in
    //     `crates/bridges/fxcm/scripts/package-fcsdk-runtime.sh`'s prose and in no code at all —
    //     a `cargo:rustc-env` that no `env!` consumes configures nothing, and had one appeared it
    //     would have baked the build box's directory into a published binary, which
    //     `scripts/refuse_box_paths.sh` refuses at TAG time.
    //
    // The whole argument for `--disable-new-dtags` moved WITH the flag rather than being deleted:
    // it is above, on the shim's own link line, with the measurement that earned it.
}
