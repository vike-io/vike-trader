use super::*;

use vike_secrets::venue_setting::VenueSettings;

/// The venue's `venue.dukascopy.demo.server` row — the shape the composition root reads out of the
/// settings database (decision 0095).
fn demo_server(value: &str) -> VenueSettings {
    VenueSettings::from_rows(
        "dukascopy",
        &[vike_secrets::VenueSettingRow {
            venue: "dukascopy".to_string(),
            tier: Some("demo".to_string()),
            field: "SERVER".to_string(),
            value: value.to_string(),
        }],
    )
}

/// The two CREDENTIAL names an account's login reads — the server is a setting since decision
/// 0095's Task 7, so it is no longer one of them.
#[test]
fn env_var_names_per_account() {
    assert_eq!(
        dukascopy_env_var_names(DukascopyAccount::Demo1),
        ("DUKASCOPY_DEMO1_LOGIN".into(), "DUKASCOPY_DEMO1_PASSWORD".into())
    );
    assert_eq!(
        dukascopy_env_var_names(DukascopyAccount::Demo2),
        ("DUKASCOPY_DEMO2_LOGIN".into(), "DUKASCOPY_DEMO2_PASSWORD".into())
    );
}

/// The prefix round trip, in BOTH spellings — the mapping a mount uses to turn a settings
/// database `account` row into a broker, so a drift between `prefix` and `key_prefix` would
/// route an order to the wrong legal entity.
#[test]
fn a_key_prefix_names_exactly_one_account_in_both_spellings() {
    for account in DukascopyAccount::ALL {
        let with = account.key_prefix();
        assert!(with.ends_with('_'), "{with} must carry the trailing separator");
        let without = with.strip_suffix('_').expect("just asserted");
        assert_eq!(DukascopyAccount::from_key_prefix(with), Some(account));
        assert_eq!(DukascopyAccount::from_key_prefix(without), Some(account));
        // …and the prefix really is the prefix of this account's own key names, so the two
        // derivations cannot drift.
        let (login, password) = dukascopy_env_var_names(account);
        for name in [login, password] {
            assert!(name.starts_with(with), "{name} must start with {with}");
        }
    }
    // Distinct, which is the whole point: one prefix, one broker.
    assert_ne!(DukascopyAccount::Demo1.key_prefix(), DukascopyAccount::Demo2.key_prefix());
    assert_ne!(DukascopyAccount::Demo1.broker(), DukascopyAccount::Demo2.broker());
}

/// ⚠ **A prefix that names neither account is `None`, never Demo1.** The fallback is the defect:
/// Demo1 is the Swiss bank, so silently coercing an unrecognised row onto it routes an order to
/// a counterparty nobody chose.
/// ⚠ Every near-miss below is BUILT from `key_prefix` at run time rather than spelled as a
/// literal. Not style: `crates/vike-ops/tests/settings_registry.rs` harvests env-SHAPED string
/// literals out of this tree and demands a registry row for each, so a hand-spelled
/// `"DUKASCOPY_DEMO3_"` in a negative test would be a variable this workspace claims to read.
#[test]
fn an_unrecognised_prefix_refuses_rather_than_defaulting() {
    let demo1 = DukascopyAccount::Demo1.key_prefix();
    for prefix in [
        demo1.replace('1', "3"),        // a third index nothing provisions
        demo1.replace('1', ""),         // the family with no index at all
        demo1.replace("DEMO1", "LIVE"), // a tier that has no arm
        demo1.to_lowercase(),           // case is not repaired
        demo1.trim_end_matches(['1', '_']).to_string(), // the venue alone
        String::new(),
        "_".to_string(),
    ] {
        assert_eq!(
            DukascopyAccount::from_key_prefix(&prefix),
            None,
            "{prefix:?} names no dukascopy account and must not resolve to one"
        );
    }
}

#[test]
fn gate_and_no_password_leak() {
    let mut vars = HashMap::new();
    let none = VenueSettings::default();
    assert!(load_dukascopy_config_from(DukascopyAccount::Demo1, &vars, &none).is_none());
    vars.insert("DUKASCOPY_DEMO1_LOGIN".into(), "DEMO2cGyrc".into());
    vars.insert("DUKASCOPY_DEMO1_PASSWORD".into(), "s3cr3t".into());
    let server = demo_server("https://demo-login.dukascopy.com/web-platform/");
    let c = load_dukascopy_config_from(DukascopyAccount::Demo1, &vars, &server).unwrap();
    assert_eq!(c.login, "DEMO2cGyrc");
    assert!(c.server.contains("dukascopy"));
    assert!(!format!("{c:?}").contains("s3cr3t"));
}

/// Decision 0095, Task 7: BOTH demo accounts read the one `venue.dukascopy.demo.server` row, and a
/// server left in the credential map under its legacy name is read by nothing (the boot refuses
/// such a store). Composed rather than spelled, so the settings registry's literal sweep does not
/// read a variable here.
#[test]
fn both_demo_accounts_read_the_tiers_server_setting() {
    let row = demo_server("https://jnlp.example.invalid/demo.jnlp");
    for account in DukascopyAccount::ALL {
        let (login_k, pass_k) = dukascopy_env_var_names(account);
        let vars = HashMap::from([(login_k, "l".to_string()), (pass_k, "p".to_string())]);
        let c = load_dukascopy_config_from(account, &vars, &row).unwrap();
        assert_eq!(c.server, "https://jnlp.example.invalid/demo.jnlp", "{account:?}");
    }
    let vars = HashMap::from([
        ("DUKASCOPY_DEMO1_LOGIN".to_string(), "l".to_string()),
        ("DUKASCOPY_DEMO1_PASSWORD".to_string(), "p".to_string()),
        (concat!("DUKASCOPY", "_DEMO1_SERVER").to_string(), "from-the-map".to_string()),
    ]);
    let c = load_dukascopy_config_from(DukascopyAccount::Demo1, &vars, &VenueSettings::default())
        .unwrap();
    assert_eq!(c.server, "", "a credential-map server is read by nothing");

    // The server is TIER-scoped: a machine-scoped (`any`) row is not the demo tier's value.
    let any = VenueSettings::from_rows(
        "dukascopy",
        &[vike_secrets::VenueSettingRow {
            venue: "dukascopy".to_string(),
            tier: None,
            field: "SERVER".to_string(),
            value: "https://jnlp.example.invalid/any.jnlp".to_string(),
        }],
    );
    let c = load_dukascopy_config_from(DukascopyAccount::Demo1, &vars, &any).unwrap();
    assert_eq!(c.server, "", "a machine-scoped server row is not read");
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
    let planted = scratch.plant_java(&format!("{PROJECT_BIN_DIR}/{JRE_TOOL_DIR}"), "jdk-17-jre");

    let tools = resolve_dukascopy_tools(&HashMap::new(), Some(&project_bin), None);

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

    let a = resolve_dukascopy_tools(&HashMap::new(), Some(&one), None);
    let b = resolve_dukascopy_tools(&HashMap::new(), Some(&two), None);

    assert_ne!(a.bridge_jar, b.bridge_jar);
    assert!(a.bridge_jar.starts_with(&one) && b.bridge_jar.starts_with(&two));
}

/// **The JForex home comes from the STATE parameter and from nothing else** — including, and
/// especially, not from `project_bin`.
///
/// The two directories are siblings on an ordinary box, so a resolver that answered
/// `project_bin.parent().join("settings/state/jforex")` would pass any test that planted them
/// side by side. This one plants them in DIFFERENT TREES, which is what `$VIKE_SETTINGS_DIR`
/// does to a real deployment: that variable moves `settings/` without moving `bin/`. A
/// `project_bin`-derived answer lands under `bin-tree/` here and the assertions below say so.
#[test]
fn the_jforex_home_follows_the_state_directory_not_the_tool_directory() {
    let scratch = Scratch::new("jforex-home");
    let project_bin = scratch.path().join("bin-tree").join(PROJECT_BIN_DIR);
    let project_state = scratch.path().join("state-tree").join("settings").join("state");

    let tools = resolve_dukascopy_tools(&HashMap::new(), Some(&project_bin), Some(&project_state));

    assert_eq!(tools.jforex_home, Some(project_state.join(JFOREX_HOME_SUBDIR)));
    let home = tools.jforex_home.expect("just asserted");
    assert!(
        !home.starts_with(&project_bin),
        "the home must not be derived from the tool directory ({}), but got {}",
        project_bin.display(),
        home.display()
    );
    // ...and nothing on this path is created: the resolver resolves, like its two siblings.
    assert!(!home.exists(), "{} must not be created by a resolver", home.display());
}

/// **No state directory, no override.** Rung 3 of the home column is a `None` rather than a
/// guess, and that is a different KIND of last rung from the jar's CWD-relative path and the
/// JVM's bare `java` — see [`resolve_dukascopy_tools`]' table. A box with no project above its
/// working directory is a developer's, where `$HOME/JForex` is not the defect this field
/// exists for.
#[test]
fn without_a_state_directory_the_jvm_keeps_its_own_home() {
    let scratch = Scratch::new("no-state");
    let project_bin = scratch.path().join(PROJECT_BIN_DIR);

    assert_eq!(
        resolve_dukascopy_tools(&HashMap::new(), Some(&project_bin), None).jforex_home,
        None
    );
    assert_eq!(resolve_dukascopy_tools(&HashMap::new(), None, None).jforex_home, None);
}

/// **A path with spaces (and, on Windows, a drive letter) survives verbatim.**
///
/// This is the resolver half of the Windows concern; the argv half is
/// `crates/bridges/dukascopy/src/exec.rs`'s `the_home_flag_is_one_argv_element_whatever_the_path`
/// — together they say that `C:\Program Files\…` reaches the JVM as ONE argument with nothing
/// quoted, split or escaped by us. Nothing here may normalise, quote or reject a path: the
/// operator chose the project root, and a resolver that "repaired" it would resolve a
/// directory nobody named.
#[test]
fn a_state_path_with_spaces_reaches_the_home_unaltered() {
    let state = Path::new("C:/Program Files/vike trader/settings/state");
    let tools = resolve_dukascopy_tools(&HashMap::new(), None, Some(state));
    assert_eq!(tools.jforex_home, Some(state.join(JFOREX_HOME_SUBDIR)));
    let rendered = tools.jforex_home.expect("just asserted").display().to_string();
    assert!(rendered.contains("Program Files"), "{rendered}");
    assert!(rendered.contains("vike trader"), "{rendered}");
    assert!(!rendered.contains('"'), "the resolver must not quote: {rendered}");
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
    let named = resolve_dukascopy_tools(&env, Some(&project_bin), None);
    assert_eq!(named.bridge_jar, PathBuf::from("/opt/somewhere/else.jar"));

    for blank in ["", "   ", "\t"] {
        env.insert(JFOREX_BRIDGE_JAR_ENV.to_string(), blank.to_string());
        assert_eq!(
            resolve_dukascopy_tools(&env, Some(&project_bin), None).bridge_jar,
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
        resolve_dukascopy_tools(&env, Some(&project_bin), None).java,
        home_exe.display().to_string()
    );

    env.insert(JAVA_HOME_ENV.to_string(), scratch.path().join("gone").display().to_string());
    assert_eq!(
        resolve_dukascopy_tools(&env, Some(&project_bin), None).java,
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
    let tools = resolve_dukascopy_tools(&HashMap::new(), None, None);

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
    assert_eq!(resolve_dukascopy_tools(&HashMap::new(), Some(&project_bin), None).java, "java");

    let rel = format!("{PROJECT_BIN_DIR}/{JRE_TOOL_DIR}");
    scratch.plant_java(&rel, "jdk-17.0.19+10-jre");
    let newer = scratch.plant_java(&rel, "jdk-21.0.2+13-jre");
    assert_eq!(
        resolve_dukascopy_tools(&HashMap::new(), Some(&project_bin), None).java,
        newer.display().to_string()
    );
}
