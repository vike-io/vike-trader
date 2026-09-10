//! Dukascopy JForex login config, read from the gitignored `.env`, **plus the two runtime tool
//! paths the sidecar is spawned with** ([`DukascopyTools`]).
//!
//! Dukascopy has no HMAC/token REST auth — its trading API is JForex (login/password/server).
//! Two demo accounts exist: DEMO1 (Dukascopy Bank SA, Swiss) and DEMO2 (Dukascopy Europe, EU).
//! Absent login/password → `None` (the live gate). The password never reaches Debug/Display.
//!
//! # Why the TOOL paths live here rather than in `exec.rs`
//!
//! Everything in this module is a PURE parser over a map the caller supplies — the shape
//! `crates/vike-ops/tests/settings_registry.rs` calls `Layer::Injected` and names as the target
//! state ("libraries take configuration as parameters; only binaries read the process
//! environment"). [`resolve_dukascopy_tools`] is that shape applied to the jar and the JVM, and it
//! is the whole of the fix for the defect `exec.rs`'s module doc used to describe: those two paths
//! were resolved by the library itself, through `concat!(env!("CARGO_MANIFEST_DIR"), …)` — the tree
//! the binary was COMPILED in, which a deployed binary does not have. Both were
//! `Layer::Library` rows in `vike_ops::settings::SETTINGS`; they are `Layer::Injected` rows now,
//! and the `LIBRARY_PIN` ratchet shrank by two.
//!
//! The caller supplies the environment map AND the already-resolved `<project>/bin` directory
//! ([`vike_model::state_path::project_bin_dir_from`]) — this module performs no walk, reads no
//! environment and probes the filesystem only where it must (an existence test is the difference
//! between a `JAVA_HOME` that names a JVM and one that names a stale directory).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_model::state_path::PROJECT_BIN_DIR;

/// Which provisioned Dukascopy demo account.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DukascopyAccount {
    /// Dukascopy Bank SA (Swiss / global).
    Demo1,
    /// Dukascopy Europe IBS AS (EU).
    Demo2,
}

impl DukascopyAccount {
    fn prefix(self) -> &'static str {
        match self {
            DukascopyAccount::Demo1 => "DUKASCOPY_DEMO1",
            DukascopyAccount::Demo2 => "DUKASCOPY_DEMO2",
        }
    }
}

/// JForex session parameters for one account.
#[derive(Clone)]
pub struct DukascopyConfig {
    pub login: String,
    pub password: String,
    /// JForex platform host (e.g. the demo web-platform server).
    pub server: String,
}

impl std::fmt::Debug for DukascopyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak the password
        write!(f, "DukascopyConfig(login={}, server={})", self.login, self.server)
    }
}

/// The `(login, password, server)` env-var names for a Dukascopy account.
pub fn dukascopy_env_var_names(account: DukascopyAccount) -> (String, String, String) {
    let p = account.prefix();
    (format!("{p}_LOGIN"), format!("{p}_PASSWORD"), format!("{p}_SERVER"))
}

/// Read Dukascopy config from a var map. `None` when login or password is unset/blank (the live
/// gate). Server may be empty (Phase 2 resolves the jnlp host when it wires the sidecar).
pub fn load_dukascopy_config_from(
    account: DukascopyAccount,
    vars: &HashMap<String, String>,
) -> Option<DukascopyConfig> {
    let (login_k, pass_k, server_k) = dukascopy_env_var_names(account);
    let get = |k: &str| vars.get(k).map(|v| v.trim().to_string()).unwrap_or_default();
    let login = get(&login_k);
    let password = get(&pass_k);
    if login.is_empty() || password.is_empty() {
        return None;
    }
    Some(DukascopyConfig { login, password, server: get(&server_k) })
}

/// The variable that names the sidecar jar OUTRIGHT, skipping the project rung: `JFOREX_BRIDGE_JAR`.
pub const JFOREX_BRIDGE_JAR_ENV: &str = "JFOREX_BRIDGE_JAR";

/// The variable that names the JVM's home directory: `JAVA_HOME`. Third-party/OS-owned, and
/// honoured before the project's own JRE for exactly that reason — an operator who set it means it.
pub const JAVA_HOME_ENV: &str = "JAVA_HOME";

/// `jforex` — the sidecar's directory under `<project>/bin`, holding [`BRIDGE_JAR_FILE`].
///
/// A directory rather than a loose jar, per [`PROJECT_BIN_DIR`]'s rule: every tool under `bin/`
/// owns a subdirectory, because a tool is a directory's worth of files even when only one of them
/// is executable.
pub const JFOREX_TOOL_DIR: &str = "jforex";

/// `jre` — the RUNTIME JVM's directory under `<project>/bin`, holding one unpacked Temurin image
/// (`bin/jre/jdk-<v>-jre/bin/java`).
///
/// ⚠ **A JRE, and deliberately not the JDK.** The split is by WHEN the image is needed, which is
/// the same rule that put this whole directory beside `settings/` instead of in `vendor/`: a JRE
/// merely RUNS the sidecar jar, so it is a RUNTIME artifact and must live where a deployed binary
/// can find it; the JDK exists for `javac` on a `--rebuild`, so it is BUILD-TIME and stays in the
/// checkout's `vendor/tools/`. `crates/bridges/dukascopy/scripts/provision-jforex.sh` writes each
/// image to its own root, and `crates/bridges/dukascopy/tests/jdk_pin_gate.rs`'s
/// `the_runtime_jre_and_the_buildtime_jdk_never_share_a_directory` holds them apart.
pub const JRE_TOOL_DIR: &str = "jre";

/// The sidecar jar's file name inside [`JFOREX_TOOL_DIR`].
pub const BRIDGE_JAR_FILE: &str = "jforex-bridge.jar";

/// Platform java executable name.
const JAVA_EXE: &str = if cfg!(windows) { "java.exe" } else { "java" };

/// The two runtime artifacts `DukascopyExecutionClient::spawn` needs, RESOLVED — never a rule the
/// exec client re-runs for itself.
///
/// Produced by [`resolve_dukascopy_tools`] and passed down as a parameter, so the only code that
/// decides where a jar or a JVM lives is the composition root that already swept the environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DukascopyTools {
    /// The program to spawn — an absolute path to a `java` executable, or the bare name `java`
    /// when the last rung (PATH lookup) is all that is left.
    pub java: String,
    /// The sidecar jar to run. Its existence is the caller's check, not this module's: a missing
    /// jar is the live gate (stay paper), and the path is wanted in the log either way.
    pub bridge_jar: PathBuf,
}

/// Resolve both runtime artifacts from a caller-supplied environment map and an already-resolved
/// `<project>/bin` directory.
///
/// PURE with respect to configuration: `env` is the caller's `std::env::vars()` sweep and
/// `project_bin` is [`vike_model::state_path::project_bin_dir_from`]'s answer. Nothing here reads
/// the process environment or performs the project walk — that is the binary's job, and it is what
/// makes a deployed binary resolve the PROJECT's tools rather than the build tree's.
///
/// # The two ladders
///
/// | | jar | JVM |
/// |---|---|---|
/// | 1 | [`JFOREX_BRIDGE_JAR_ENV`] | [`JAVA_HOME_ENV`]`/bin/java`, when that file EXISTS |
/// | 2 | `<project>/bin/jforex/jforex-bridge.jar` | highest-sorting `<project>/bin/jre/*/bin/java` |
/// | 3 | the same path, CWD-relative (no project found) | `java`, from `PATH` |
///
/// Rung 2 is what replaced the compile-time `vendor/` fallbacks, and rung 3 is a genuine last
/// resort rather than a tidy default — with no project above the working directory there is no
/// better answer, and rung 1 names the artifact outright for anyone in that position. (Same shape,
/// and the same argument, as `crates/vike-research/src/bin/research.rs`'s `default_lightgbm` —
/// a DELETED binary, gone with the research crate, so the citation is the evidence for the shape
/// rather than a file to go and read; it is filed in `crates/vike-ops/tests/citation_gate.rs`'s
/// `DEAD_PATH_EXCEPTIONS`.)
///
/// ⚠ **A blank value is IGNORED rather than honoured**, the rule every `_from` resolver in
/// [`vike_model::state_path`] follows: an empty `Environment=JFOREX_BRIDGE_JAR=` line would
/// otherwise resolve the jar to `""`, and the venue would degrade to paper reporting a path that
/// names nothing. `JAVA_HOME` already had this guard; the jar did not.
///
/// ⚠ **`JAVA_HOME` must actually CONTAIN a JVM to win.** A set-but-stale value falls through to
/// the project's own JRE instead of failing the spawn — preserved verbatim from the resolution
/// this replaced, because a leftover `JAVA_HOME` on a dev box is common and is not a decision.
pub fn resolve_dukascopy_tools(
    env: &HashMap<String, String>,
    project_bin: Option<&Path>,
) -> DukascopyTools {
    DukascopyTools {
        java: resolve_java(env, project_bin),
        bridge_jar: resolve_bridge_jar(env, project_bin),
    }
}

/// [`resolve_dukascopy_tools`]' jar ladder — see its table.
fn resolve_bridge_jar(env: &HashMap<String, String>, project_bin: Option<&Path>) -> PathBuf {
    if let Some(named) = non_blank(env.get(JFOREX_BRIDGE_JAR_ENV)) {
        return PathBuf::from(named);
    }
    let bin = project_bin.map_or_else(|| PathBuf::from(PROJECT_BIN_DIR), Path::to_path_buf);
    bin.join(JFOREX_TOOL_DIR).join(BRIDGE_JAR_FILE)
}

/// [`resolve_dukascopy_tools`]' JVM ladder — see its table.
fn resolve_java(env: &HashMap<String, String>, project_bin: Option<&Path>) -> String {
    if let Some(home) = non_blank(env.get(JAVA_HOME_ENV)) {
        let exe = PathBuf::from(home).join("bin").join(JAVA_EXE);
        if exe.is_file() {
            return exe.display().to_string();
        }
    }
    if let Some(jre) = project_bin.and_then(|bin| java_under(&bin.join(JRE_TOOL_DIR))) {
        return jre;
    }
    // The bare name, NOT `JAVA_EXE`: `std::process::Command` appends the platform suffix itself,
    // and this rung is a PATH lookup rather than a path.
    "java".to_string()
}

/// A map value, trimmed, or `None` when unset, empty or whitespace-only.
///
/// Takes the ALREADY-LOOKED-UP value rather than the map and a key, deliberately: that keeps each
/// `.get(CONST)` at the call site, where `crates/vike-ops/tests/settings_registry.rs`'s
/// `find_lookup_sites` can resolve the constant and PROVE the read is a map lookup. A key handed to
/// a helper has no call syntax to anchor on and lands in that gate's measured blind spot instead —
/// a spelling choice worth two lines, since these two rows are exactly the ones that just left the
/// `LIBRARY_PIN` work-list and the claim is that they are now injected.
fn non_blank(value: Option<&String>) -> Option<&str> {
    value.map(|v| v.trim()).filter(|v| !v.is_empty())
}

/// Highest-sorting `<dir>/*/bin/java` (e.g. `bin/jre/jdk-17.0.19+10-jre/bin/java`).
///
/// One level of nesting, because Temurin unpacks into a versioned directory. Highest-sorting so a
/// second image left by a version bump is preferred over the one it replaced.
///
/// ⚠ The image-mixing incident this workspace paid for once (`provision-jforex.sh`'s `find_java`)
/// cannot recur HERE: a JDK sorts before its own `-jre` sibling, but only JREs are ever written
/// under [`JRE_TOOL_DIR`] — the JDK lives in the checkout's `vendor/tools/`, which this sweep
/// cannot see at all.
fn java_under(dir: &Path) -> Option<String> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join("bin").join(JAVA_EXE))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found.pop().map(|p| p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_names_per_account() {
        assert_eq!(
            dukascopy_env_var_names(DukascopyAccount::Demo1),
            (
                "DUKASCOPY_DEMO1_LOGIN".into(),
                "DUKASCOPY_DEMO1_PASSWORD".into(),
                "DUKASCOPY_DEMO1_SERVER".into()
            )
        );
        assert_eq!(
            dukascopy_env_var_names(DukascopyAccount::Demo2),
            (
                "DUKASCOPY_DEMO2_LOGIN".into(),
                "DUKASCOPY_DEMO2_PASSWORD".into(),
                "DUKASCOPY_DEMO2_SERVER".into()
            )
        );
    }

    #[test]
    fn gate_and_no_password_leak() {
        let mut vars = HashMap::new();
        assert!(load_dukascopy_config_from(DukascopyAccount::Demo1, &vars).is_none());
        vars.insert("DUKASCOPY_DEMO1_LOGIN".into(), "DEMO2cGyrc".into());
        vars.insert("DUKASCOPY_DEMO1_PASSWORD".into(), "s3cr3t".into());
        vars.insert(
            "DUKASCOPY_DEMO1_SERVER".into(),
            "https://demo-login.dukascopy.com/web-platform/".into(),
        );
        let c = load_dukascopy_config_from(DukascopyAccount::Demo1, &vars).unwrap();
        assert_eq!(c.login, "DEMO2cGyrc");
        assert!(c.server.contains("dukascopy"));
        assert!(!format!("{c:?}").contains("s3cr3t"));
    }

    /// A private scratch directory. This crate has no `tempfile` dev-dependency and this change
    /// adds no dependency, so the filesystem tests below make (and remove) their own — the same
    /// `Scratch` shape `crates/vike-model/src/state_path.rs` uses for the walk tests these mirror.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!("vike-duka-tools-{tag}-{nanos}"));
            std::fs::create_dir_all(&p).expect("scratch dir");
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        /// A fake unpacked Java image at `<scratch>/<rel>/<version>/bin/java[.exe]`.
        fn plant_java(&self, rel: &str, version: &str) -> PathBuf {
            let bin = self.0.join(rel).join(version).join("bin");
            std::fs::create_dir_all(&bin).expect("image dir");
            let exe = bin.join(JAVA_EXE);
            std::fs::write(&exe, b"").expect("java stub");
            exe
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// **THE property this whole change exists for**: with nothing named in the environment, both
    /// artifacts resolve under the PROJECT directory the caller passed — never a path baked in at
    /// compile time.
    ///
    /// Asserted as `starts_with(project_bin)` rather than against a literal, because the failure
    /// being guarded is not "the sub-directory was renamed" but "the answer came from somewhere
    /// else entirely" — which is what `concat!(env!("CARGO_MANIFEST_DIR"), …)` did, and what a
    /// deployed binary experienced as a venue silently on paper.
    #[test]
    fn both_tools_resolve_under_the_project_the_caller_named() {
        let scratch = Scratch::new("project");
        let project_bin = scratch.path().join(PROJECT_BIN_DIR);
        let planted =
            scratch.plant_java(&format!("{PROJECT_BIN_DIR}/{JRE_TOOL_DIR}"), "jdk-17-jre");

        let tools = resolve_dukascopy_tools(&HashMap::new(), Some(&project_bin));

        assert_eq!(tools.bridge_jar, project_bin.join(JFOREX_TOOL_DIR).join(BRIDGE_JAR_FILE));
        assert_eq!(tools.java, planted.display().to_string());
        assert!(
            Path::new(&tools.java).starts_with(&project_bin),
            "the JVM must come from the project ({}), not {}",
            project_bin.display(),
            tools.java
        );
        // ...and NOT from this crate's own manifest directory, which is what the deleted
        // `bridge_jar`/`java_program` resolved through.
        let build_tree = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(!tools.bridge_jar.starts_with(build_tree), "{}", tools.bridge_jar.display());
        assert!(!Path::new(&tools.java).starts_with(build_tree), "{}", tools.java);
    }

    /// Moving the project moves BOTH artifacts with it — the tool twin of
    /// `the_settings_override_moves_the_tool_dir_with_it` in `vike_model::state_path`, asserted one
    /// layer up where the paths are actually consumed. A resolver that answered from anywhere but
    /// its `project_bin` argument would agree with itself across two different projects.
    #[test]
    fn a_different_project_gives_different_paths() {
        let scratch = Scratch::new("two-projects");
        let one = scratch.path().join("one").join(PROJECT_BIN_DIR);
        let two = scratch.path().join("two").join(PROJECT_BIN_DIR);

        let a = resolve_dukascopy_tools(&HashMap::new(), Some(&one));
        let b = resolve_dukascopy_tools(&HashMap::new(), Some(&two));

        assert_ne!(a.bridge_jar, b.bridge_jar);
        assert!(a.bridge_jar.starts_with(&one) && b.bridge_jar.starts_with(&two));
    }

    /// Rung 1 wins over the project, and a BLANK value does not.
    ///
    /// The blank half is the new guard: `std::env::var` returns `Ok("")` for
    /// `Environment=JFOREX_BRIDGE_JAR=`, so the resolution this replaced accepted `""` as a path
    /// and reported "bridge jar not found" naming nothing.
    #[test]
    fn the_named_jar_beats_the_project_but_a_blank_one_does_not() {
        let scratch = Scratch::new("jar-env");
        let project_bin = scratch.path().join(PROJECT_BIN_DIR);
        let project_default = project_bin.join(JFOREX_TOOL_DIR).join(BRIDGE_JAR_FILE);

        let mut env = HashMap::new();
        env.insert(JFOREX_BRIDGE_JAR_ENV.to_string(), "/opt/somewhere/else.jar".to_string());
        let named = resolve_dukascopy_tools(&env, Some(&project_bin));
        assert_eq!(named.bridge_jar, PathBuf::from("/opt/somewhere/else.jar"));

        for blank in ["", "   ", "\t"] {
            env.insert(JFOREX_BRIDGE_JAR_ENV.to_string(), blank.to_string());
            assert_eq!(
                resolve_dukascopy_tools(&env, Some(&project_bin)).bridge_jar,
                project_default,
                "a blank {JFOREX_BRIDGE_JAR_ENV} must fall through to the project"
            );
        }
    }

    /// `JAVA_HOME` outranks the project's JRE — but only when it actually holds a JVM. A stale
    /// value falls through rather than failing the spawn, which is verbatim the behaviour of the
    /// resolution this replaced.
    #[test]
    fn java_home_wins_only_when_it_really_holds_a_jvm() {
        let scratch = Scratch::new("java-home");
        let project_bin = scratch.path().join(PROJECT_BIN_DIR);
        let project_jre =
            scratch.plant_java(&format!("{PROJECT_BIN_DIR}/{JRE_TOOL_DIR}"), "jdk-17-jre");
        let home_exe = scratch.plant_java("elsewhere", "jdk-21");
        let home = home_exe.parent().and_then(Path::parent).expect("<home>/bin/java");

        let mut env = HashMap::new();
        env.insert(JAVA_HOME_ENV.to_string(), home.display().to_string());
        assert_eq!(
            resolve_dukascopy_tools(&env, Some(&project_bin)).java,
            home_exe.display().to_string()
        );

        env.insert(JAVA_HOME_ENV.to_string(), scratch.path().join("gone").display().to_string());
        assert_eq!(
            resolve_dukascopy_tools(&env, Some(&project_bin)).java,
            project_jre.display().to_string(),
            "a stale JAVA_HOME must fall through to the project's own JRE"
        );
    }

    /// No project, nothing named: rung 3. The jar answers CWD-relative (the only honest answer
    /// left) and the JVM is the bare PATH name — never a path into whichever checkout compiled the
    /// binary, which is exactly what the two deleted `concat!(env!("CARGO_MANIFEST_DIR"), …)`
    /// fallbacks produced.
    #[test]
    fn without_a_project_the_last_resort_is_relative_never_the_build_tree() {
        let tools = resolve_dukascopy_tools(&HashMap::new(), None);

        assert_eq!(
            tools.bridge_jar,
            Path::new(PROJECT_BIN_DIR).join(JFOREX_TOOL_DIR).join(BRIDGE_JAR_FILE)
        );
        assert!(tools.bridge_jar.is_relative(), "{}", tools.bridge_jar.display());
        assert_eq!(tools.java, "java");
        assert!(!tools.bridge_jar.starts_with(env!("CARGO_MANIFEST_DIR")));
    }

    /// The JRE sweep takes the highest-sorting image, so a version bump that left the old one
    /// behind still resolves the new one; an absent directory falls through to `PATH`.
    #[test]
    fn the_jre_sweep_picks_the_highest_image_or_falls_through() {
        let scratch = Scratch::new("jre-sweep");
        let project_bin = scratch.path().join(PROJECT_BIN_DIR);

        // Nothing planted yet: the directory does not exist at all.
        assert_eq!(resolve_dukascopy_tools(&HashMap::new(), Some(&project_bin)).java, "java");

        let rel = format!("{PROJECT_BIN_DIR}/{JRE_TOOL_DIR}");
        scratch.plant_java(&rel, "jdk-17.0.19+10-jre");
        let newer = scratch.plant_java(&rel, "jdk-21.0.2+13-jre");
        assert_eq!(
            resolve_dukascopy_tools(&HashMap::new(), Some(&project_bin)).java,
            newer.display().to_string()
        );
    }
}
