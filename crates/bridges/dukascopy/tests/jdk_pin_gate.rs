//! The JDK pin is spelled in four repo files, and they must agree.
//!
//! `javac` output differs across JDK releases, so a given `jforex-bridge.jar` is only reproducible
//! against ONE Temurin release. `.github/workflows/jforex-bridge.yml` builds the jar twice and
//! requires the two to be byte-identical, so a pin skew reddens that gate on a PR that changed no
//! Java — and the failure lands on whoever touched the bridge, not on whoever bumped a version.
//!
//! ⚠ **The fourth pin is the one that now ships bytes to strangers.** The jar is no longer
//! committed: `.github/workflows/release.yml` builds it from these sources and ATTACHES it to the
//! GitHub Release, checksummed in that release's `SHA256SUMS`. So the release's own `java-version`
//! decides what people DOWNLOAD, while the gate's decides what CI PROVED — and if those two drift,
//! the gate reproduces a jar nobody has, the release publishes a jar nothing checked, and both
//! workflows stay green. That failure has no other detector, which is why the release joined this
//! gate in the same change that made it a publisher.
//!
//! # Why a gate instead of a sentence
//!
//! `crates/bridges/dukascopy/CLAUDE.md` used to say "bumping the JDK is a TWO-PLACE change" and
//! then, two sentences later, describe three places. A hand count of pins is the exact thing this
//! tree keeps paying for. The pins are:
//!
//!   1. **CI** — `.github/workflows/jforex-bridge.yml`'s `java-version`, the toolchain the
//!      reproducibility gate actually builds with. Spelled WITHOUT the Temurin build number
//!      (`17.0.19`).
//!   2. **THE RELEASE** — `.github/workflows/release.yml`'s `java-version`, the toolchain that
//!      compiles the jar people install. Same spelling as (1).
//!   3. **Linux/macOS provisioning** — `deploy/jre/provision-jre.sh`'s `JDK_VERSION`, spelled WITH
//!      it (`17.0.19+10`), plus a per-OS/arch/image `SHA256` table the download is verified against
//!      BEFORE extraction.
//!   4. **Windows provisioning** — `crates/bridges/dukascopy/scripts/provision-jforex.ps1`'s
//!      `$JdkVersion` and its `$JdkSha256`.
//!
//! ⚠ Pin 2 MOVED out of `crates/bridges/dukascopy/scripts/provision-jforex.sh` and into `deploy/`,
//! and this gate moved with it. Two things forced that: a SECOND consumer of the identical Temurin
//! build appeared (the IBKR Client Portal Gateway, whose runbook carried an unverified `curl | tar`
//! for it), and a provisioner under `crates/` cannot run on an INSTALLED project — the CI box's is
//! `bin/ data/ settings/ user_data/` and no source tree, measured 2026-08-22. The sidecar's script
//! now CALLS the provisioner, which is what
//! [`the_sidecar_provisioner_delegates_instead_of_carrying_a_second_pin`] holds: a fourth pin
//! growing back where the third used to be is exactly what this file exists to refuse.
//!
//! Follow a stale sentence and a clean box provisions the PREVIOUS JDK, whose `javac` then produces
//! a jar that does not match what the release shipped. That is why the version equality is asserted
//! here rather than written down anywhere.
//!
//! # The checksums, and the honest limit
//!
//! A version bump also invalidates every pinned `SHA256`: they are per-RELEASE digests, so the
//! bumped script either refuses to extract (checksum mismatch) or — if the URL was left behind too —
//! silently provisions the old JDK. This gate CANNOT verify a digest without the network, so it
//! checks the next best thing, which is where the digests came from: each script cites its Adoptium
//! `assets/version/<version>` source URL beside the table, and that URL must name the version the
//! script pins. Refetching the checksums and updating that URL is one action; forgetting both is
//! what this catches.
//!
//! ⚠ **The fourth pin is out of reach on purpose**: the JDK already unzipped under a dev box's
//! gitignored `vendor/tools/`. Nothing in a checkout can see it — replace it by hand.
//!
//! # The adjacent promise: `--rebuild` must actually GET a JDK
//!
//! A pinned version buys nothing if the script never downloads it. `provision-jre.sh` resolves
//! Java BEFORE the download block, and its `find_java` used to take the highest-sorting
//! `vendor/tools/*/bin/java` regardless of image — so `--rebuild` on a box that already had a JRE
//! from an earlier plain run reused it, downloaded nothing, and handed gradle a `javac`-less
//! runtime. The failure then surfaced three steps later as a gradle error naming neither the JRE nor
//! the script, and the fix ("delete the JRE directory by hand") lived only in prose.
//!
//! **That incident is now closed twice over, and the two guards are not redundant.**
//!
//!   1. [`the_runtime_jre_and_the_buildtime_jdk_never_share_a_directory`] — the STRUCTURAL half, and
//!      the primary one. The two images no longer live in one place: the JRE merely RUNS the
//!      fetched jar, so it is a RUNTIME artifact and goes to `<project>/bin/jre/`, while the JDK
//!      exists for `javac` under `--rebuild`, so it is BUILD-TIME and stays in `vendor/tools/`. A
//!      JRE from an earlier plain run is therefore not on `--rebuild`'s search path AT ALL, and no
//!      sort order can put it there. (The same split is why the jar itself moved to
//!      `bin/jforex/` — `vendor/` exists only in a checkout, so nothing a deployed binary needs may
//!      live there.)
//!   2. [`rebuild_discovery_refuses_a_javac_less_runtime`] — the BEHAVIOURAL half, kept because the
//!      two roots are a few lines of shell apart and a future edit could point them at one directory
//!      again. Cheap, and it is the guard that would still catch that edit.
//!
//! ⚠ The `.ps1` sibling used to need no equivalent — it had no JRE path at all, because Windows
//! always provisioned the JDK. That is no longer true: it now provisions a JRE by default (the
//! runtime a Windows box needs once the exec client stopped resolving `vendor/tools`) and the JDK
//! only under `-Force`. Guard 1 covers BOTH scripts; guard 2 remains shell-only because the `.ps1`
//! expresses its javac check as a pipeline filter rather than as a named function this gate can
//! carve out of the text.
//!
//! ⚠ Honest limit, same shape as the checksum one above: this is a check on the script's TEXT.
//! Nothing in this repo EXECUTES either provisioning script — they download 44-190 MB from
//! Adoptium — so no test proves the runtime behaviour, and this one only proves the guard was not
//! deleted.

use std::path::{Path, PathBuf};

/// Workspace root from `CARGO_MANIFEST_DIR` (never CWD) — the `citation_gate.rs` /
/// `settings_registry.rs` idiom, so the gate does not care where `cargo` was invoked from.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
}

const WORKFLOW: &str = ".github/workflows/jforex-bridge.yml";
/// The publishing half. Since the jar stopped being committed this workflow BUILDS the one people
/// download, so its toolchain pin is as load-bearing as the gate's.
const RELEASE: &str = ".github/workflows/release.yml";
/// THE Linux/macOS provisioner — the one that downloads, verifies and extracts. It left
/// `crates/bridges/dukascopy/scripts/` because a second consumer needed the identical pin and
/// because a provisioner under `crates/` cannot run on an installed project.
const SH: &str = "deploy/jre/provision-jre.sh";
/// Its caller: the sidecar's own script, which must carry NO pin of its own.
const JFOREX: &str = "crates/bridges/dukascopy/scripts/provision-jforex.sh";
const PS1: &str = "crates/bridges/dukascopy/scripts/provision-jforex.ps1";

/// The gradle task both workflows must invoke. `shadowJar` is what produces the fat
/// `jforex-bridge-all.jar` the sidecar actually is; `jar` alone produces a manifest-only archive
/// that starts and immediately dies on a missing JForex class.
const SHADOW_TASK: &str = "shadowJar";

fn read(rel: &str) -> String {
    let p = workspace_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| {
        panic!(
            "jdk_pin_gate cannot read {rel} ({e}). If the file MOVED, re-point this gate at it — \
             do not delete the row: an unreadable pin is an unchecked pin."
        )
    })
}

/// The text between the first pair of `'` or `"` after `key` on the line that declares it.
///
/// Deliberately literal: all three pins are a quoted scalar on one line, and a parser that
/// tolerated more shapes would also tolerate the pin drifting into a shape nothing reads.
fn quoted_after(src: &str, rel: &str, key: &str) -> String {
    let line = src.lines().find(|l| l.contains(key)).unwrap_or_else(|| {
        panic!("{rel} no longer declares `{key}` — the JDK pin moved or was renamed")
    });
    let rest = &line[line.find(key).expect("checked by find") + key.len()..];
    let start = rest
        .find(['\'', '"'])
        .unwrap_or_else(|| panic!("{rel}'s `{key}` is not a quoted scalar: {line}"));
    let quote = rest.as_bytes()[start] as char;
    let tail = &rest[start + 1..];
    let end = tail
        .find(quote)
        .unwrap_or_else(|| panic!("{rel}'s `{key}` has an unterminated quote: {line}"));
    tail[..end].to_string()
}

/// `17.0.19+10` -> `17.0.19`. CI pins the release, the scripts pin the release+build that Adoptium's
/// download API addresses.
fn base(version: &str) -> &str {
    version.split('+').next().expect("split always yields one")
}

/// Every `<major>.<minor>.<patch>` token in `src`, each with any `+NN` / `%2BNN` build suffix kept.
/// Used to prove that a bump left no copy of the OLD version behind in a comment or a URL.
fn version_tokens(src: &str) -> Vec<String> {
    let bytes: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let starts_token = i == 0
            || (!bytes[i - 1].is_ascii_digit() && bytes[i - 1] != '.' && bytes[i - 1] != '%');
        if bytes[i].is_ascii_digit() && starts_token {
            let start = i;
            let mut dots = 0;
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == '.') {
                if bytes[j] == '.' {
                    // a trailing dot ends a sentence, not a version
                    if j + 1 >= bytes.len() || !bytes[j + 1].is_ascii_digit() {
                        break;
                    }
                    dots += 1;
                }
                j += 1;
            }
            if dots >= 2 {
                let mut end = j;
                // keep an Adoptium build suffix, raw (`+10`) or URL-encoded (`%2B10`)
                if end < bytes.len() && bytes[end] == '+' {
                    let mut k = end + 1;
                    while k < bytes.len() && bytes[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k > end + 1 {
                        end = k;
                    }
                } else if end + 3 < bytes.len()
                    && bytes[end] == '%'
                    && bytes[end + 1] == '2'
                    && bytes[end + 2].eq_ignore_ascii_case(&'B')
                {
                    let mut k = end + 3;
                    while k < bytes.len() && bytes[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k > end + 3 {
                        end = k;
                    }
                }
                out.push(bytes[start..end].iter().collect::<String>());
                i = end;
                continue;
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    out
}

/// CI, the RELEASE, the shell script and the PowerShell script must pin ONE Temurin release.
///
/// This is the assertion the prose used to make and get wrong. It fails naming every pin, so the
/// repair is mechanical rather than a hunt.
#[test]
fn every_jdk_pin_in_the_repo_agrees() {
    let wf = quoted_after(&read(WORKFLOW), WORKFLOW, "java-version:");
    let rel = quoted_after(&read(RELEASE), RELEASE, "java-version:");
    let sh = quoted_after(&read(SH), SH, "JDK_VERSION=");
    let ps1 = quoted_after(&read(PS1), PS1, "$JdkVersion");

    assert_eq!(
        sh, ps1,
        "the two provisioning scripts pin different Temurin releases — {SH} says `{sh}`, {PS1} says \
         `{ps1}`. A clean box then gets a different JDK depending on its OS, and only one of them \
         builds the jar the release shipped."
    );
    assert_eq!(
        wf,
        base(&sh),
        "CI and the provisioning scripts pin different Temurin releases — {WORKFLOW} builds the \
         reproducibility-gated jar with `{wf}` while the scripts provision `{sh}`. Whoever \
         provisions with the scripts will produce a jar CI rejects, on a PR that changed no Java."
    );
    assert_eq!(
        rel, wf,
        "the RELEASE and the gate pin different Temurin releases — {RELEASE} compiles the jar \
         people DOWNLOAD with `{rel}` while {WORKFLOW} proves reproducibility with `{wf}`. Both \
         workflows stay green: the gate reproduces a jar nobody has, and the release publishes \
         bytes nothing checked. Nothing else can see this."
    );
    assert!(
        wf.split('.').count() >= 3 && wf.chars().next().is_some_and(|c| c.is_ascii_digit()),
        "{WORKFLOW}'s java-version parsed as `{wf}`, which is not a version — this gate is reading \
         the wrong thing and would pass vacuously."
    );
}

/// The release must BUILD the artifact this gate proves, with the same gradle task.
///
/// A pin equality over a workflow that no longer builds anything would be a green tick over
/// nothing: the interesting failure is not "the release names a different JDK" but "the release
/// stopped producing the jar at all", after which `scripts/fetch_release_tools.sh` reports a
/// missing asset to every fresh install and this gate carries on passing.
///
/// Deliberately a TEXT check on two workflow files. Nothing in this repository runs gradle from a
/// Rust test — the module doc's honest limit, and it applies here for the same reason.
#[test]
fn the_release_builds_the_jar_this_gate_proves() {
    let release = read(RELEASE);
    let workflow = read(WORKFLOW);

    assert!(
        release.contains(SHADOW_TASK),
        "{RELEASE} no longer runs gradle's `{SHADOW_TASK}`, so a release publishes no sidecar jar. \
         Dukascopy's ONLY execution path then has no obtainable artifact: the jar is not committed \
         any more, and `scripts/fetch_release_tools.sh` can only report a missing asset. If the \
         build moved to another workflow, re-point this gate at it."
    );
    assert!(
        workflow.contains(SHADOW_TASK),
        "{WORKFLOW} no longer runs gradle's `{SHADOW_TASK}` — the reproducibility gate is not \
         building the artifact the release ships, so nothing proves the published bytes are \
         re-derivable from the tag."
    );
    assert!(
        release.contains("jforex-bridge.jar"),
        "{RELEASE} builds `{SHADOW_TASK}` but never names `jforex-bridge.jar`, so the jar is \
         compiled and then not staged for upload. The asset name is the contract \
         `scripts/fetch_release_tools.sh` and `crates/bridges/dukascopy/scripts/provision-jforex.ps1` \
         both look up by name."
    );
    // The gate's own path filter is what makes any of this fire on a release-only edit: the Rust
    // gates are selected from changed SOURCE and PROSE, never from YAML, so a PR touching only
    // release.yml runs no cargo test at all. Without the filter this assertion is unreachable
    // exactly when it is needed.
    assert!(
        workflow.contains(RELEASE),
        "{WORKFLOW} no longer path-filters on `{RELEASE}`. A PR that edits only the release \
         workflow selects no Rust crate (`xtask::ci::gate_crates_for` keys on source and prose), \
         so THIS test would not run on the change it exists to judge, and the jar gate would not \
         re-prove the build either."
    );
}

/// A version bump must not leave the OLD version behind in a comment or a checksum-source URL.
///
/// The `SHA256` digests beside those URLs are per-release; a URL still naming the previous release
/// is the visible half of "the checksums were never refetched", which is the failure the reviewer
/// of #1187 predicted would follow a hand-counted pin list.
#[test]
fn no_provisioning_script_mentions_a_stale_jdk_version() {
    for rel in [SH, PS1] {
        let src = read(rel);
        let pin = quoted_after(&src, rel, if rel == SH { "JDK_VERSION=" } else { "$JdkVersion" });
        let stale: Vec<String> = version_tokens(&src)
            .into_iter()
            .filter(|t| {
                let normalized = t.replace("%2B", "+").replace("%2b", "+");
                normalized != pin && base(&normalized) != base(&pin)
            })
            .collect();
        assert!(
            stale.is_empty(),
            "{rel} pins JDK `{pin}` but also mentions {stale:?}. Every version-shaped token in a \
             provisioning script refers to the JDK it provisions — a leftover is either a stale \
             'must match CI' comment or, worse, a checksum-source URL naming the release the \
             pinned SHA256 table was fetched from. Re-fetch the checksums from \
             https://api.adoptium.net/v3/assets/version/<pinned version> and update both."
        );
    }
}

/// `--rebuild` asks for `IMAGE=jdk`; the Java the script then REUSES must be able to compile.
///
/// The guard is two lines inside `provision-jre.sh`'s `find_java` — skip any candidate whose
/// `bin/javac` is missing while `IMAGE` is `jdk`. Delete them and the script silently goes back to
/// promising a JDK in its header and provisioning whatever happens to sort last.
#[test]
fn rebuild_discovery_refuses_a_javac_less_runtime() {
    let src = read(SH);
    let start = src.find("find_java()").unwrap_or_else(|| {
        panic!(
            "{SH} no longer defines `find_java` — Java discovery moved, re-point this gate at it"
        )
    });
    let body_end = src[start..].find("\n}").map_or(src.len(), |o| start + o);
    let body = &src[start..body_end];
    assert!(
        body.contains("javac"),
        "{SH}'s `find_java` no longer mentions javac, so a JRE left by an earlier plain run again \
         satisfies `--rebuild`: the script skips the JDK download its own header promises and hands \
         gradle a runtime that cannot compile. Restore the IMAGE=jdk guard.\n--- find_java ---\n{body}"
    );
    assert!(
        body.contains("$IMAGE") || body.contains("${IMAGE}"),
        "{SH}'s `find_java` mentions javac but not IMAGE, so the guard is unconditional — the \
         ordinary JRE path would now reject its own runtime.\n--- find_java ---\n{body}"
    );
    // ⚠ The SELECTION lives in the CALLER now, not here. `provision-jre.sh` takes `--image`, so the
    // literal this used to look for in one file is split across two: the sidecar script decides
    // that `--rebuild` means a JDK, the provisioner enforces that a JDK can compile. Both halves are
    // asserted, because either one alone is satisfiable while the promise is broken — a caller that
    // stopped asking for a JDK would leave the guard below green with nothing to guard.
    let jforex = read(JFOREX);
    assert!(
        jforex.contains("IMAGE=jdk"),
        "{JFOREX} no longer selects IMAGE=jdk for --rebuild, so the javac guard in {SH} has nothing \
         to key on: a rebuild would ask for the runtime image and hand gradle a javac-less JVM."
    );
    assert!(
        jforex.contains("--image"),
        "{JFOREX} computes an IMAGE and never passes `--image` to {SH}, so the provisioner falls \
         back to its `jre` default and --rebuild silently provisions the wrong image."
    );
    assert!(
        src.contains("--image"),
        "{SH} no longer accepts `--image`, so its caller's selection reaches nothing."
    );
}

/// The sidecar's provisioner DELEGATES: it may not grow a second pin back.
///
/// This is the shape the module doc warns about. `provision-jforex.sh` carried the table until the
/// IBKR gateway needed the same Temurin build; if a future edit inlines a download "just for the
/// sidecar", the repo is back to two pins that agree only until one is bumped — with `every_jdk_pin
/// _in_the_repo_agrees` unable to see the new one, because it reads the file this test keeps empty.
#[test]
fn the_sidecar_provisioner_delegates_instead_of_carrying_a_second_pin() {
    let jforex = read(JFOREX);
    for (needle, what) in [
        ("SHA256=", "a checksum table"),
        ("JDK_VERSION=", "a version pin"),
        ("api.adoptium.net", "an Adoptium download URL"),
        ("tar -xzf", "an extraction step"),
    ] {
        assert!(
            !jforex.contains(needle),
            "{JFOREX} declares `{needle}` — {what} that belongs to {SH} alone. The download, the \
             pin and the verify-before-extract moved there so ONE file answers 'which Temurin', and \
             a copy here is a second pin that agrees only until somebody bumps one of them."
        );
    }
    assert!(
        jforex.contains("provision-jre.sh"),
        "{JFOREX} names no provisioner, so it obtains a JVM some other way — the delegation is the \
         whole reason it may carry no pin."
    );
}

/// The provisioner is reachable from an INSTALLED project, not only from a checkout.
///
/// ⚠ This is the constraint that forced the move, and it is invisible from inside a source tree
/// where every path happens to exist. the CI box's project root holds `bin/ data/ settings/ user_data/`
/// and no `crates/`, so a provisioner under `crates/` is a file that box does not have. `deploy/`
/// is copied to a target as part of the install; `crates/` never is.
#[test]
fn the_provisioner_lives_where_an_installed_project_can_reach_it() {
    assert!(
        SH.starts_with("deploy/"),
        "the Temurin provisioner is `{SH}`, outside `deploy/`. An installed project has no source \
         tree — it cannot run a script under `crates/`, which is the same reason a runtime artifact \
         may not live under `vendor/` (`vike_model::state_path::PROJECT_BIN_DIR`)."
    );
    assert!(
        !read(SH).contains("CARGO_MANIFEST_DIR"),
        "{SH} resolves a path through CARGO_MANIFEST_DIR — the tree it was BUILT in, which an \
         installed project does not have. It resolves `<project>` through \
         `deploy/vike-tool-root.sh` instead."
    );
}

/// The RUNTIME image and the BUILD-TIME image must resolve from two DIFFERENT directories, in both
/// scripts — the structural half of the `--rebuild` promise (see this file's module doc).
///
/// Asserted on the two root ASSIGNMENTS plus the fact that the download EXTRACTS into a
/// variable root rather than a fixed one. Both halves are load-bearing: declaring two roots and
/// then extracting into `$TOOLS` unconditionally would put a JRE back under the JDK's directory
/// while every other assertion here still passed, which is precisely the shape of the original
/// defect (a promise made in the header and not kept by the code below it).
///
/// ⚠ This is a check on the scripts' TEXT, the same honest limit the checksum tests carry: nothing
/// in this repo EXECUTES either script — they download 44-190 MB from Adoptium — so no test proves
/// the runtime behaviour, only that the split was not undone.
#[test]
fn the_runtime_jre_and_the_buildtime_jdk_never_share_a_directory() {
    // (file, jdk-root key, jre-root key, the variable the extraction must land in)
    let scripts =
        [(SH, "TOOLS=", "JRE_DIR=", "$IMAGE_ROOT"), (PS1, "$Tools ", "$JreDir ", "$ImageRoot")];
    for (rel, jdk_key, jre_key, extract_var) in scripts {
        let src = read(rel);
        let jdk_root = quoted_after(&src, rel, jdk_key);
        let jre_root = quoted_after(&src, rel, jre_key);

        assert_ne!(
            jdk_root.replace('\\', "/"),
            jre_root.replace('\\', "/"),
            "{rel} points the build-time JDK and the runtime JRE at ONE directory (`{jdk_root}`). \
             A JRE left by an earlier plain run is then back on --rebuild's search path, which is \
             the incident this split closed — see this gate's module doc."
        );
        assert!(
            jdk_root.replace('\\', "/").contains("vendor/tools"),
            "{rel}'s build-time root is `{jdk_root}`, not vendor/tools. The JDK is javac's, i.e. \
             BUILD-time, and must not be installed into the project's runtime tool directory — a \
             deployed project would then carry a 190 MB compiler it never runs."
        );
        assert!(
            jre_root.replace('\\', "/").contains("bin/jre"),
            "{rel}'s runtime root is `{jre_root}`, not bin/jre. The exec client resolves \
             `<project>/bin/jre/*/bin/java` at RUNTIME (see \
             `crates/bridges/dukascopy/src/config.rs`'s `resolve_dukascopy_tools`); an image \
             anywhere else is invisible to it."
        );
        assert!(
            src.contains(extract_var),
            "{rel} declares two roots but never mentions `{extract_var}`, so the download cannot be \
             extracting into the root belonging to the image it just fetched. Two roots that the \
             extraction ignores are a comment, not a split."
        );
    }
}

/// The checksum tables must EXIST, so the test above cannot pass by finding nothing to check.
///
/// Same shape as the `the_gate_actually_sees_the_tree` floors elsewhere in this repo: a gate whose
/// input silently became empty is worse than no gate, because it reports green.
#[test]
fn each_provisioning_script_still_verifies_its_download() {
    for (rel, needle) in [(SH, "SHA256="), (PS1, "$JdkSha256")] {
        let src = read(rel);
        let digests = src.matches(needle).count();
        assert!(
            digests > 0,
            "{rel} no longer declares `{needle}` — the download is unverified, and \
             no_provisioning_script_mentions_a_stale_jdk_version has nothing left to key on."
        );
        assert!(
            src.contains("api.adoptium.net/v3/assets/version/"),
            "{rel} declares {digests} checksum(s) but no longer cites the Adoptium \
             assets/version URL they were fetched from — that citation is the only evidence \
             available offline that they match the pinned release."
        );
    }
}
