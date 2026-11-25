//! The one program that answers "what does CI run".
//!
//! Invoked as `cargo run -p xtask -- <cmd>`, wrapped by `just xtask <cmd>` so nobody types that.
//! ⚠ **There is deliberately no `cargo xtask` alias**, which is the usual convention: an alias lives
//! in `.cargo/config.toml`, `xtask/tests/build_config_gate.rs` requires every leaf path in
//! that file to sit under `target.x86_64-pc-windows-msvc`, and cargo does not allow `[alias]` to be
//! target-scoped — so no spelling of the convention passes the gate that protects the Linux boxes
//! holding the credential store. The `justfile` recipe is the entry point instead.
//!
//! One consumer each, and no command without one: a command nobody consumes is a surface that rots
//! unobserved, which is the defect this crate was written to remove. (The count that used to open
//! this paragraph said "three" while the table listed four, so the table is the roster and no
//! sentence restates its length.)
//!
//! | command | consumer | output |
//! |---|---|---|
//! | `crates` | the `justfile`'s roster recipes | one line, `-p a -p b …` |
//! | `doc-crates` | `scripts/build_api_docs.sh` | one line, `-p a -p b …` |
//! | `affected` | `.github/workflows/{ci,release}.yml`'s `plan` step | the `key=value` lines |
//! | `feature-lanes` | a human, and the gates in `crates/vike-ops/tests/` | `key<TAB>trigger,trigger` |
//! | `docs-bins` | `release.yml`'s docs-data step | `bins.json` |
//! | `sdk-report [--renames FILE]` | the CI step `docs/ops/sdk-release.md` names | markdown, and the rename table into `FILE` |
//! | `sdk-renames FROM_TAG FILE` | `scripts/package_strategy_source.sh`, at an SDK's tag | the step's rename table into `FILE` |
//! | `sdk-boundary` | the deploy's boundary step, `docs/ops/sdk-release.md` | the `key=value` lines `xtask::sdk::Boundary::render` writes |
//! | `sdk-cut [--dry-run] [--no-push]` | `just sdk-cut` | what it did, in words |
//!
//! `sdk-report` and `sdk-boundary` append to `$GITHUB_STEP_SUMMARY` and `$GITHUB_OUTPUT` the same way.
//! `sdk-report` is informational: it never fails a job, and a report it cannot make says why on the page.
//!
//! `affected` appends its block to `$GITHUB_OUTPUT` when that variable is set, as well as printing
//! it. Writing the file itself — rather than leaving the workflow to redirect stdout — is what keeps
//! the `plan` step a plain `run:` line with no shell plumbing to get wrong, and it means a stray
//! `cargo` line on stdout could never become a job output.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use xtask::ci;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    if let Err(e) = run(cmd, args.get(1..).unwrap_or_default()) {
        eprintln!("xtask {cmd}: {e}");
        std::process::exit(1);
    }
}

/// The repository root. `cargo run` sets the working directory to the workspace root, and
/// `CARGO_MANIFEST_DIR` is this crate's own directory — its parent — so neither depends on where the
/// caller was standing. The env var wins because a `cargo xtask` invoked from a crate directory keeps
/// that directory as the CWD.
fn repo_root() -> PathBuf {
    match std::env::var_os("CARGO_MANIFEST_DIR") {
        Some(dir) => PathBuf::from(dir).join(".."),
        None => std::env::current_dir().expect("a working directory"),
    }
}

/// Append `body` to the file the variable `var` names, when it is set (a workflow step's output or
/// summary file).
fn append_to(env: &BTreeMap<String, String>, var: &str, body: &str) -> Result<(), String> {
    if let Some(dest) = env.get(var).filter(|d| !d.is_empty()) {
        let mut fh = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dest)
            .map_err(|e| format!("could not open ${var} ({dest}): {e}"))?;
        fh.write_all(body.as_bytes()).map_err(|e| format!("could not write ${var}: {e}"))?;
    }
    Ok(())
}

fn run(cmd: &str, args: &[String]) -> Result<(), String> {
    let root = repo_root();
    let env: BTreeMap<String, String> = std::env::vars().collect();
    match cmd {
        // The same string as the plan's `roster=` line, through the same two functions — so the
        // justfile's roster recipes and ci.yml's `test` job build ONE feature resolution.
        "crates" => {
            let g = ci::graph::load_graph(&root)?;
            println!("{}", ci::roster::package_args(&ci::roster::ci_crates(&g.names)));
        }
        // The PUBLISHED API reference's roster. It prints the same string as `crates` today, and
        // that equality is asserted by `ci::docs_roster_tests` rather than assumed — but the two
        // answer DIFFERENT QUESTIONS ("what does the merge gate compile" and "what goes on a public
        // page"), and `scripts/build_api_docs.sh` calling this one is what makes `ci::roster::doc_crates`
        // the seam three pages already call it. Before this command existed that function had no
        // consumer, so a divergence written into it would have changed nothing.
        "doc-crates" => {
            let g = ci::graph::load_graph(&root)?;
            println!("{}", ci::roster::package_args(&ci::roster::doc_crates(&g.names)));
        }
        "affected" => {
            let plan = ci::plan::compute(&env, &root)?;
            // To STDERR: stdout is the machine-readable surface. Without this line a plan is
            // unfalsifiable after the fact — the 40-file sweep of PR #1139 was only diagnosable
            // because someone read the checkout step's log by hand.
            eprintln!("{}", plan.diagnostic);
            // INCREMENTAL or FULL, and why (decision 0278): also the run's summary page, where it is read.
            eprintln!("{} {}", ci::DIAG_PREFIX, plan.summary);
            // Why the shipped-strategies check is out of this plan, when it is.
            if !plan.strategy_lab.is_empty() {
                eprintln!("{} {}", ci::DIAG_PREFIX, plan.strategy_lab);
            }
            let text = plan.render();
            let summary = format!("{}\n", plan.summary);
            append_to(&env, "GITHUB_OUTPUT", &text)?;
            append_to(&env, "GITHUB_STEP_SUMMARY", &summary)?;
            print!("{text}");
        }
        // Informational (`xtask::sdk::report`): a report it cannot make is said on the page, never a red job.
        "sdk-report" => {
            let renames = match args {
                [] => None,
                [flag, file] if flag == "--renames" => Some(PathBuf::from(file)),
                _ => return Err("usage: sdk-report [--renames FILE]".into()),
            };
            let page = match xtask::sdk::sdk_report(&root)
                .and_then(|r| Ok((r, xtask::sdk::boundary(&root)?)))
            {
                Ok((None, boundary)) => {
                    format!(
                        "## Strategy SDK\n\nNo `sdk-*` tag yet: nothing to compare. {}.\n",
                        boundary.sentence()
                    )
                }
                Ok((Some(report), boundary)) => {
                    if let Some(file) = &renames {
                        std::fs::write(file, report.rename_table().render())
                            .map_err(|e| format!("could not write {}: {e}", file.display()))?;
                    }
                    report.markdown(&boundary.sentence())
                }
                Err(e) => format!("## Strategy SDK\n\nThe SDK report could not be made: {e}\n"),
            };
            append_to(&env, "GITHUB_STEP_SUMMARY", &page)?;
            print!("{page}");
        }
        // ONE step's rename table, for the packer at an SDK's tag (`scripts/package_strategy_source.sh`): the names
        // the tree of `FROM` (the SDK before) has and this tree does not. Unlike `sdk-report`, a table it cannot make
        // is an error: a snapshot shipped without its step's renames would migrate nothing.
        "sdk-renames" => {
            let [from, file] = args else {
                return Err("usage: sdk-renames FROM_TAG FILE".into());
            };
            let table = xtask::sdk::report_from(from, &root)?.rename_table();
            std::fs::write(file, table.render())
                .map_err(|e| format!("could not write {file}: {e}"))?;
        }
        "sdk-boundary" => {
            let boundary = xtask::sdk::boundary(&root)?;
            eprintln!("{}", boundary.sentence());
            let text = boundary.render();
            append_to(&env, "GITHUB_OUTPUT", &text)?;
            append_to(&env, "GITHUB_STEP_SUMMARY", &format!("{}\n", boundary.sentence()))?;
            print!("{text}");
        }
        "sdk-cut" => {
            let options = xtask::sdk::cut::CutOptions::parse(args)?;
            let done = xtask::sdk::cut::cut(&root, options, |root| {
                // Check 5: every shipped strategy file is a module of the lab's test build.
                let status = std::process::Command::new("cargo")
                    .args(["test", "-p", ci::tables::roster::STRATEGY_LAB_CRATE, "--no-run"])
                    .current_dir(root)
                    .status()
                    .map_err(|e| format!("could not run cargo: {e}"))?;
                if status.success() {
                    Ok(())
                } else {
                    Err("the shipped strategies do not compile at HEAD: no SDK is cut".to_string())
                }
            })?;
            println!("{done}");
        }
        "feature-lanes" => {
            for (key, triggers) in ci::tables::feature_suites::FEATURE_SUITES {
                println!("{key}\t{}", triggers.join(","));
            }
        }
        "docs-bins" => {
            // The `bins.json` docs-data release asset (`xtask::docs_bins`): pretty JSON,
            // newline-terminated, to stdout — `release.yml` redirects it beside the four files the
            // vike-ops `docs_data` bin writes.
            let meta = ci::graph::load_metadata(&root)?;
            let bins = xtask::docs_bins::bins_value(&meta)?;
            let mut out = serde_json::to_string_pretty(&bins)
                .map_err(|e| format!("bins.json did not serialize: {e}"))?;
            out.push('\n');
            print!("{out}");
        }
        other => {
            return Err(format!(
                "unknown command {other:?}. \
                 Try: crates | doc-crates | affected | feature-lanes | docs-bins | sdk-report | \
                 sdk-renames | sdk-boundary | sdk-cut"
            ));
        }
    }
    Ok(())
}
