//! ONE CI runner per box, each building and testing a FIXED half of the workspace (the owner's ruling of
//! 2026-11-22, "yes i do it"), and the runner every job of `.github/workflows/` runs on.
//!
//! **The halves are derived, never a list of crates.** A crate's half follows its lane HOME GROUP, which
//! `scripts/prod2_lane.sh`'s `crate_group` states once, as a few name patterns. This module READS that function
//! as text ([`member_halves`]): groups [`HALF_A_GROUPS`] (data and backtest, desktop and app) plus
//! [`HALF_A_CRATES`] are half A; every other group (trading and core, venues and bridges) plus [`HALF_B_CRATES`]
//! is half B. One fact, two readers (bash runs it, this module parses it), one gate:
//! `xtask/tests/ci_plan_gate/halves.rs` runs the bash function over every `[workspace].members` crate and holds
//! this reader to its answers. A lane edit this reader cannot follow is a red plan, never a silent half.
//!
//! **Balance, MEASURED by wall time on each box, not by test-seconds alone** (the `test` legs and jobs of the seven
//! `ci` runs of 2026-11-21; the per-test seconds of the six full-roster ones): group 1 without the two gate crates
//! 2,499 test-seconds, group 2 1,031, group 3 322, group 4 1,356, `vike-ops` 958, `xtask` 622, so half A is 2,312
//! and half B 4,477, 1.94x. But a box runs tests at its own rate: a leg on the latency box ran a median 8.6 test-seconds per
//! second of nextest wall, on the CI box 16.8 (6 legs each; the latency box's CI shares its CPUs with ClickHouse and the nodes at
//! `ci.slice` weight 25, load 141 measured 2026-11-22). Half A on the latency box is then ~269 s of nextest wall and half B on
//! the CI box ~266 s: even. Moving a whole group would unbalance it (group 4 to A: ~426 s against ~186 s), so none moved.
//!
//! **Each half has a SIDE runner on its box for its other heavy jobs** (the owner's "do!" of 2026-11-22, Variant B):
//! [`Route::Side`]. They ran one after another on the half runner, behind its `test` leg, with about a minute between
//! two jobs (batches #3350 and #3354: 11 and 8.5 min); beside it the estimate was ~6.5. A side runner has its own
//! persistent `target/`, built incrementally with every CPU of the box's `ci.slice` like the half's, so the test
//! build and the side jobs' builds no longer evict each other from one tree.

use std::collections::BTreeMap;
use std::path::Path;

/// The lane script whose `crate_group` is the one statement of a crate's home group.
pub const LANE_GROUP_SCRIPT: &str = "scripts/prod2_lane.sh";

/// The function of [`LANE_GROUP_SCRIPT`] this module reads.
pub const LANE_GROUP_FUNCTION: &str = "crate_group";

/// The home groups whose crates are half A (`crate_group` prints the group number).
pub const HALF_A_GROUPS: &[&str] = &["2", "3"];

/// Half A whatever their group: the `vike-ops` gates.
pub const HALF_A_CRATES: &[&str] = &["vike-ops"];

/// Half B whatever their group: the planner and its gates.
pub const HALF_B_CRATES: &[&str] = &["xtask"];

/// A half of the workspace: one box, its `test` leg on one runner with one warm `target/`, its side jobs on another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Half {
    /// Data, backtest, desktop and app, and `vike-ops`: `the build runner`, its side jobs on `the build runner`.
    A,
    /// Trading, core, venues and bridges, and `xtask`: `the build runner`, its side jobs on `the build runner`.
    B,
}

impl Half {
    pub const ALL: [Half; 2] = [Half::A, Half::B];

    /// The matrix leg's name: `test (a)`, `validate (b)`.
    pub fn name(self) -> &'static str {
        match self {
            Half::A => "a",
            Half::B => "b",
        }
    }

    /// The runner label the half's jobs ask for.
    pub fn label(self) -> &'static str {
        match self {
            Half::A => "ci-half-a",
            Half::B => "ci-half-b",
        }
    }

    /// The label of the half's side runner, on the same box: [`Route::Side`].
    pub fn side_label(self) -> &'static str {
        match self {
            Half::A => "ci-side-a",
            Half::B => "ci-side-b",
        }
    }
}

/// The label of the third CI runner (`the build runner`): jobs that compile nothing but the planner.
pub const LIGHT_LABEL: &str = "ci-light";

/// Where a job runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// That half's runner.
    On(Half),
    /// That half's side runner: the half's box, beside its `test` leg rather than behind it.
    Side(Half),
    /// One matrix leg per half, each on its half's runner (`runs-on` reads `matrix.runner`, from the plan's `halves`).
    EachHalf,
    /// The light runner.
    Light,
}

/// EVERY job of `.github/workflows/` that runs on a CI runner, by `(workflow file, job key)`. A job not here runs on a
/// runner this ruling did not touch (`the latency runner`'s `the latency box`, `the build runner`), which
/// `xtask/tests/ci_plan_gate/halves.rs` holds both ways with each `runs-on` line.
///
/// The heavy jobs of `ci.yml` go by SUBJECT (the brief of 2026-11-22): the data side and the desktop to A, the trading
/// side and the venue suites to B. `features` is wholly B: its one leg that could have gone either way,
/// `windows-cross`, went by MEASUREMENT to the half with spare time (median job walls per box, the runs above): A's
/// queue on the latency box is its ~269 s of nextest, `datahub-all` ~216 s and `app-check` ~282 s; B's on the CI box its ~266 s,
/// `tradehub-all` ~94 s and the other suites ~128 s. `windows-cross` adds ~151 s on the CI box (~204 s on the latency box), which
/// leaves B the shorter queue; each half's test build comes on top. Since the owner's "do!" of 2026-11-22 those
/// jobs run on their half's SIDE runner ([`Route::Side`]), beside its `test` leg: the queue above is now two queues
/// per box. Since the owner's go of the same day they are the STEPS of one job per side runner, `ci.yml`'s `side-a`
/// (`datahub-all`, `app-check`, `lightgbm`) and `side-b` (the suites, `tradehub-all`), with no job pickup between
/// them. A job of another workflow keeps the box it was routed to and goes by subject when it was pooled over
/// both. `release.yml`'s twins follow `ci.yml`'s, so they build in the `target/` that keeps their configuration warm.
pub const JOB_ROUTES: &[(&str, &str, Route)] = &[
    ("ci.yml", "plan", Route::Light),
    ("ci.yml", "test", Route::EachHalf),
    ("ci.yml", "side-a", Route::Side(Half::A)),
    ("ci.yml", "side-b", Route::Side(Half::B)),
    ("ci.yml", "merge-gate", Route::Light),
    ("release.yml", "preflight", Route::Light),
    ("release.yml", "plan", Route::Light),
    ("release.yml", "validate", Route::EachHalf),
    ("release.yml", "features", Route::Side(Half::B)),
    ("release.yml", "datahub-all", Route::Side(Half::A)),
    ("release.yml", "tradehub-all", Route::Side(Half::B)),
    ("agent-eval.yml", "agent-eval", Route::On(Half::B)),
    ("api-docs.yml", "api-docs", Route::On(Half::B)),
    ("brand-assets.yml", "verify-brand-assets", Route::On(Half::B)),
    ("content.yml", "gate", Route::Light),
    ("coverage.yml", "comment-only", Route::Light),
    ("coverage.yml", "coverage", Route::On(Half::A)),
    ("ctrader-proto.yml", "regen-and-verify", Route::On(Half::B)),
    ("deny.yml", "cargo-deny", Route::Light),
    ("feature-matrix.yml", "comment-only", Route::Light),
    ("feature-matrix.yml", "each-feature", Route::On(Half::B)),
    ("fonts-vendor.yml", "verify-vendored-fonts", Route::Light),
    ("fuzz.yml", "comment-only", Route::Light),
    ("fuzz.yml", "fuzz", Route::On(Half::B)),
    ("jforex-bridge.yml", "build-and-verify", Route::On(Half::B)),
    ("live-smokes.yml", "readonly-nightly", Route::On(Half::B)),
    ("live-smokes.yml", "weekly-order-lifecycle", Route::On(Half::B)),
    ("mcp-registry-publish.yml", "publish", Route::Light),
    ("mirror-publish.yml", "mirror", Route::Light),
    ("mutants.yml", "comment-only", Route::Light),
    ("mutants.yml", "mutants", Route::On(Half::A)),
    ("queue.yml", "pass", Route::Light),
];

/// The `runs-on:` line a job of `route` carries, byte for byte.
pub fn runs_on(route: Route) -> String {
    match route {
        Route::On(half) => format!("runs-on: [self-hosted, {}]", half.label()),
        Route::Side(half) => format!("runs-on: [self-hosted, {}]", half.side_label()),
        Route::EachHalf => "runs-on: [self-hosted, \"${{ matrix.runner }}\"]".to_string(),
        Route::Light => format!("runs-on: [self-hosted, {LIGHT_LABEL}]"),
    }
}

/// A crate's half from its name and its home group.
pub fn half_of(name: &str, group: &str) -> Half {
    if HALF_A_CRATES.contains(&name) {
        Half::A
    } else if HALF_B_CRATES.contains(&name) || !HALF_A_GROUPS.contains(&group) {
        Half::B
    } else {
        Half::A
    }
}

/// The argument `crate_group` takes for a member directory relative to the workspace root: `bridges/<venue>` for a
/// bridge, the directory under `crates/` for any other crate, `-` for a member outside `crates/` (`xtask`).
pub fn group_key(member_dir: &str) -> String {
    match member_dir.strip_prefix("crates/") {
        Some(rest) => match rest.strip_prefix("bridges/") {
            Some(venue) => format!("bridges/{}", venue.split('/').next().unwrap_or(venue)),
            None => rest.split('/').next().unwrap_or(rest).to_string(),
        },
        None => "-".to_string(),
    }
}

/// `crate_group`'s `case` arms out of the script's text, in order: `(patterns, group)`. `Err` for any shape this
/// reader does not know (a pattern with a character class, a quote or a `?`, an arm that does more than `echo N`).
pub fn group_arms(script: &str) -> Result<Vec<(Vec<String>, String)>, String> {
    let head = format!("{LANE_GROUP_FUNCTION}() {{");
    let mut lines = script.lines().skip_while(|l| !l.starts_with(&head));
    lines.next().ok_or_else(|| format!("{LANE_GROUP_SCRIPT} defines no `{head}`"))?;
    if lines.next().map(str::trim) != Some("case \"$1\" in") {
        return Err(format!("{LANE_GROUP_FUNCTION} no longer opens with `case \"$1\" in`"));
    }
    let mut arms = Vec::new();
    for line in lines.map(str::trim) {
        if line == "esac" {
            return Ok(arms);
        }
        let bad = || {
            format!("{LANE_GROUP_FUNCTION}'s arm `{line}` is not `pattern | pattern) echo N ;;`")
        };
        let (patterns, action) = line.split_once(')').ok_or_else(bad)?;
        let group = action
            .trim()
            .strip_prefix("echo ")
            .and_then(|a| a.strip_suffix(";;"))
            .map(str::trim)
            .filter(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(bad)?;
        let patterns: Vec<String> = patterns.split('|').map(|p| p.trim().to_string()).collect();
        let legal = |p: &String| {
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"*/._-".contains(&b))
        };
        if !patterns.iter().all(legal) {
            return Err(bad());
        }
        arms.push((patterns, group.to_string()));
    }
    Err(format!("{LANE_GROUP_FUNCTION}'s `case` has no `esac`"))
}

/// Bash `case` matching of `s` against a pattern whose only wildcard is `*` ([`group_arms`] refuses every other).
pub fn glob(pattern: &str, s: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = s.strip_prefix(first) else { return false };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else { return rest.is_empty() };
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// The group the first matching arm prints, as bash's `case` picks it.
pub fn group_of<'a>(arms: &'a [(Vec<String>, String)], key: &str) -> Option<&'a str> {
    arms.iter().find(|(patterns, _)| patterns.iter().any(|p| glob(p, key))).map(|(_, g)| g.as_str())
}

/// Every workspace member's half. `dirs` is the graph's absolute manifest directory -> package name, under `cwd`, the
/// workspace root. `Err` when the script cannot be read or parsed, or a member sits outside `cwd` or matches no arm:
/// the halves would be a guess.
pub fn member_halves(
    dirs: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<BTreeMap<String, Half>, String> {
    let script = std::fs::read_to_string(cwd.join(LANE_GROUP_SCRIPT)).map_err(|e| {
        format!("{LANE_GROUP_SCRIPT} cannot be read ({e}): the CI halves would be a guess")
    })?;
    let arms = group_arms(&script)?;
    let root = format!("{}/", super::super::graph::abspath(".", cwd));
    let mut halves = BTreeMap::new();
    for (dir, name) in dirs {
        let rel = dir.strip_prefix(&root).ok_or_else(|| {
            format!("{name}'s directory {dir} is not under {root}: its half would be a guess")
        })?;
        let key = group_key(rel);
        let group = group_of(&arms, &key).ok_or_else(|| {
            format!(
                "{name} ({key}) matches no arm of {LANE_GROUP_FUNCTION}: its half would be a guess"
            )
        })?;
        halves.insert(name.clone(), half_of(name, group));
    }
    Ok(halves)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glob_is_bash_case_with_star_alone() {
        for (p, s, want) in [
            ("bridges/*", "bridges/binance", true),
            ("bridges/*", "bridges/", true),
            ("bridges/*", "vike-bridges/x", false),
            ("vike-gatewayhub*", "vike-gatewayhub-client", true),
            ("vike-gatewayhub*", "vike-gatewayhub", true),
            ("vike-*-window", "vike-account-window", true),
            ("vike-*-window", "vike-window", false),
            ("vike-*-manager", "vike-data-manager", true),
            ("vike-data*", "vike-datahub", true),
            ("vike-data*", "vike-dat", false),
            ("vike-store", "vike-store", true),
            ("vike-store", "vike-store2", false),
            ("*", "-", true),
            ("a*b*c", "abc", true),
            ("a*b*c", "acb", false),
            ("a*a", "a", false),
        ] {
            assert_eq!(glob(p, s), want, "{p} against {s}");
        }
    }

    #[test]
    fn the_arms_are_read_in_order_and_an_unknown_shape_is_refused() {
        let script = "x() {\n}\ncrate_group() { # doc\n  case \"$1\" in\n    bridges/* | vike-venues) echo 4 ;;\n    \
                      vike-data*) echo 2 ;;\n    *) echo 1 ;;\n  esac\n}\n";
        let arms = group_arms(script).expect("parses");
        assert_eq!(group_of(&arms, "bridges/okx"), Some("4"));
        assert_eq!(group_of(&arms, "vike-datahub"), Some("2"));
        assert_eq!(group_of(&arms, "-"), Some("1"));
        for (bad, why) in [
            (script.replace("echo 2", "echo two"), "a group that is no number"),
            (script.replace("vike-data*", "vike-data[ab]"), "a character class"),
            (script.replace("vike-data*", "\"vike-data\""), "a quoted pattern"),
            (script.replace("echo 2 ;;", "echo 2; exit ;;"), "an arm that does more"),
            (script.replace("  esac\n", ""), "no esac"),
            (script.replace("crate_group()", "home_group()"), "no function"),
        ] {
            assert!(group_arms(&bad).is_err(), "{why} must be refused: {bad}");
        }
    }

    #[test]
    fn the_key_is_the_one_crate_group_takes() {
        assert_eq!(group_key("crates/vike-core"), "vike-core");
        assert_eq!(group_key("crates/bridges/binance"), "bridges/binance");
        assert_eq!(group_key("xtask"), "-");
        assert_eq!(half_of("vike-ops", "1"), Half::A);
        assert_eq!(half_of("xtask", "2"), Half::B);
        assert_eq!(half_of("vike-data", "2"), Half::A);
        assert_eq!(half_of("vike-chart", "3"), Half::A);
        assert_eq!(half_of("vike-binance", "4"), Half::B);
        assert_eq!(half_of("vike-core", "1"), Half::B);
    }
}
