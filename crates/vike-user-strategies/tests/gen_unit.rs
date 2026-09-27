//! Unit tests for the PURE generator (`vike_user_strategies::codegen`) on synthetic trees — the exact
//! code `build.rs` runs (it `include!`s the same file). Temp trees are built with std only (no
//! tempfile dep): a unique dir under the target-local temp root, removed at the end of each test.

use std::fs;
use std::path::PathBuf;

use vike_user_strategies::codegen::{ScannedStrategy, render, scan, valid_name};

/// A scratch user_data root, unique per test, cleaned on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!("vike-user-strategies-gen-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn strategy(&self, name: &str, with_entry: bool, manifest: Option<&str>) {
        let d = self.0.join("strategies").join("rust").join(name);
        fs::create_dir_all(&d).unwrap();
        if with_entry {
            fs::write(d.join(format!("{name}.rs")), "// entry\n").unwrap();
        }
        if let Some(m) = manifest {
            fs::write(d.join("strategy.toml"), m).unwrap();
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn absent_root_scans_empty_without_errors() {
    let out = scan(std::path::Path::new("Z:/definitely/absent/user_data"));
    assert!(out.strategies.is_empty());
    assert!(out.errors.is_empty());
}

#[test]
fn scan_finds_entries_skips_presets_only_and_reads_live() {
    let s = Scratch::new("happy");
    s.strategy("alpha", true, None);
    s.strategy("beta", true, Some("live = true\n"));
    s.strategy("built_in_presets", false, None); // presets-only: skipped silently
    let out = scan(&s.0);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let names: Vec<&str> = out.strategies.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta"], "sorted, presets-only folder absent");
    assert!(!out.strategies[0].live && out.strategies[1].live);
}

#[test]
fn bad_name_and_bad_manifest_are_errors_naming_the_path() {
    let s = Scratch::new("errors");
    s.strategy("Bad-Name", true, None);
    s.strategy("okay", true, Some("live = \"yes\"\n")); // non-boolean live
    let out = scan(&s.0);
    assert!(out.strategies.is_empty());
    assert_eq!(out.errors.len(), 2, "{:?}", out.errors);
    assert!(out.errors.iter().any(|e| e.contains("Bad-Name")));
    assert!(out.errors.iter().any(|e| e.contains("okay") && e.contains("live")));
}

#[test]
fn valid_name_charset() {
    assert!(valid_name("abs_v2"));
    assert!(!valid_name("Abs"));
    assert!(!valid_name("2fast"));
    assert!(!valid_name("dash-ed"));
    assert!(!valid_name(""));
}

#[test]
fn render_empty_and_nonempty_shapes() {
    let empty = render(&[]);
    assert!(empty.contains("USER_STRATEGIES: &[&str] = &[]"));
    assert!(empty.contains("let _ = (name, params);"));

    let one = render(&[ScannedStrategy {
        name: "abs".into(),
        entry: PathBuf::from(r"C:\some\user_data\strategies\rust\abs\abs.rs"),
        live: true,
    }]);
    assert!(
        one.contains(r#"#[path = "C:/some/user_data/strategies/rust/abs/abs.rs"]"#),
        "backslashes rendered forward: {one}"
    );
    assert!(one.contains("pub mod user_abs;"));
    assert!(one.contains(r#"USER_LIVE_CAPABLE: &[&str] = &["abs"]"#));
    assert!(one.contains(r#""abs" => Some(user_abs::build::<B>(params)),"#));
}
