//! `vike-nlp` — the command line of the text models, a release tool of its own (the owner's ruling of
//! 2026-11-16: neither the datahub nor `vike-backend` links a model).
//!
//! - `vike-nlp fetch [--model NAME]...` downloads the pinned models (every one when no `--model` is
//!   given) into `<project>/models/<name>/<revision>/`, prints each licence first, and verifies every
//!   file's sha256; a second run re-verifies and downloads only what is missing or wrong.
//! - `vike-nlp score --from YYYY-MM-DD --to YYYY-MM-DD [--model NAME]... [--collector rss]` reads the
//!   NEWS the datahub recorded in that window over the datahub wire (`config.datahub_addr`, this box's
//!   device key), scores every text with each model, and writes one file per UTC day into
//!   `<project>/market_data/imports/text-scores/<MODEL>/` (the model's name upper-cased, the import's dataset
//!   grammar; a day whose file is there is kept). Its last
//!   lines name the `vike-cli data history import` that stores them. A window reaching into a day that
//!   has not ended is refused: a day is scored once, whole. Run it on the DATAHUB's box: the import
//!   reads that box's imports folder.
//!
//! The project is the one `vike_boot::boot` finds (`$VIKE_SETTINGS_DIR`, else the walk from the
//! working directory). Exit 0 on success, 2 on a command-line error, 1 on any other failure (the
//! message on standard error).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use vike_data::text_log::TEXT_KIND_NEWS;
use vike_data::text_score_log::{TEXT_SCORE_FORMAT, text_score_dataset};
use vike_datahub_client::route::{history_route, open_routed_history, routed_hub_keys};
use vike_model::MS_PER_DAY;
use vike_nlp::pins::{PINNED_MODELS, PinnedModel, pinned};
use vike_nlp::score_texts::{Scorer, day_file, score_rows, texts_by_day, write_day_file};

const BIN: &str = "vike-nlp";

#[derive(Parser)]
#[command(about = "Fetch the pinned text models and score the stored news with them")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download the pinned models into <project>/models/ and verify every file's sha256.
    Fetch {
        /// A model to fetch, by name; repeat for several. Default: every pinned model.
        #[arg(long = "model")]
        models: Vec<String>,
    },
    /// Score the news received in a window into day files the datahub's `text-scores` import reads.
    Score {
        /// The first UTC day scored, YYYY-MM-DD.
        #[arg(long)]
        from: String,
        /// The last UTC day scored, YYYY-MM-DD; it must have ended.
        #[arg(long)]
        to: String,
        /// A model to score with, by name; repeat for several. Default: every pinned model.
        #[arg(long = "model")]
        models: Vec<String>,
        /// The collector of the news series, the datahub recorder's word
        /// (`crates/vike-datahub/src/tool_feeds/record.rs`'s `NEWS_COLLECTOR`).
        #[arg(long, default_value = "rss")]
        collector: String,
    },
}

fn main() -> ExitCode {
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let args: Args = vike_utility::cli::Program::new(BIN, &version).parse();
    let env: HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    let booted = match boot(&env, cwd.as_deref()) {
        Ok(booted) => booted,
        Err(why) => {
            eprintln!("{BIN}: {why}");
            return ExitCode::FAILURE;
        }
    };
    for warning in &booted.settings.warnings {
        eprintln!("{BIN}: {warning}");
    }
    let result = match project_dirs(&booted) {
        Err(why) => Err(why),
        Ok((settings_dir, models_dir)) => match args.command {
            Command::Fetch { models } => fetch(&models_dir, &models),
            Command::Score { from, to, models, collector } => {
                let window = Window { from: &from, to: &to, collector: &collector };
                score(&booted, &settings_dir, &models_dir, &window, &models)
            }
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{BIN}: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The startup: the project walk and the settings, for `config.datahub_addr`.
fn boot(env: &HashMap<String, String>, cwd: Option<&Path>) -> Result<vike_boot::Booted, String> {
    vike_boot::boot(&vike_boot::BootSpec {
        env,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Refuse,
        settings: vike_boot::SettingsLoad::Load,
        credentials: vike_boot::Credentials::DeferredNoVenueKeys(
            "it reads no credential; its one read of the settings database is this box's device key",
        ),
        log_home: vike_boot::LogHome::Elsewhere(
            "a one-shot command that builds no subscriber: it writes its own lines to the terminal",
        ),
        disclosure: vike_boot::Disclosure::Skip("a one-shot command, not a daemon starting"),
    })
}

/// `<project>/settings` and `<project>/models`, or the refusal of a run with no project.
fn project_dirs(booted: &vike_boot::Booted) -> Result<(PathBuf, PathBuf), String> {
    let settings_dir = booted.settings_dir.clone();
    let models_dir = vike_paths::state_path::models_dir_beside(settings_dir.as_deref());
    match (settings_dir, models_dir) {
        (Some(settings), Some(models)) => Ok((settings, models)),
        _ => {
            Err("no project found: run it inside a project folder (the one holding `settings/`), \
                  or name its settings folder with VIKE_SETTINGS_DIR"
                .to_string())
        }
    }
}

fn model_named(name: &str) -> Result<&'static PinnedModel, String> {
    pinned(name).ok_or_else(|| {
        let known: Vec<&str> = PINNED_MODELS.iter().map(|m| m.name).collect();
        format!("no pinned model is named {name:?}; the pinned models are {}", known.join(", "))
    })
}

/// The named pinned models, or every one when none is named.
fn models_of(names: &[String]) -> Result<Vec<&'static PinnedModel>, String> {
    if names.is_empty() {
        return Ok(PINNED_MODELS.iter().collect());
    }
    names.iter().map(|name| model_named(name)).collect()
}

fn fetch(models_dir: &Path, names: &[String]) -> Result<(), String> {
    for model in models_of(names)? {
        println!(
            "{}: {} at {} (licence {}; fetched for your own use, never redistributed)",
            model.name, model.repository, model.revision, model.license
        );
        let folder = vike_nlp::fetch::fetch(model, models_dir).map_err(|e| e.to_string())?;
        println!("{}: verified in {}", model.name, folder.display());
    }
    Ok(())
}

/// The window `score` was given, as typed.
struct Window<'a> {
    from: &'a str,
    to: &'a str,
    collector: &'a str,
}

fn score(
    booted: &vike_boot::Booted,
    settings_dir: &Path,
    models_dir: &Path,
    window: &Window<'_>,
    names: &[String],
) -> Result<(), String> {
    let models = models_of(names)?;
    let day_of = |label: &str| {
        vike_data::text_score_log::text_score_day(label)
            .ok_or_else(|| format!("{label:?} is not a day: give YYYY-MM-DD"))
    };
    let (first, last) = (day_of(window.from)?, day_of(window.to)?);
    let now_ms = now_ms();
    if first > last {
        return Err(format!("--from {} is after --to {}", window.from, window.to));
    }
    if last + MS_PER_DAY > now_ms {
        return Err(format!(
            "--to {} has not ended yet: a day is scored once, whole, so end the window at an ended day",
            window.to
        ));
    }
    let imports = vike_paths::state_path::imports_dir_beside(Some(settings_dir))
        .ok_or("no imports folder: the settings folder has no project above it")?;

    let route = history_route(booted.settings.config.datahub_addr.as_deref());
    let keys = routed_hub_keys(vike_secrets::access::device_key_load(settings_dir), route.hub());
    let store = open_routed_history(&route, keys);
    let range = vike_data::TimestampRange::of(first, last + MS_PER_DAY - 1);
    let texts = store
        .scan_texts(TEXT_KIND_NEWS, window.collector, range)
        .map_err(|e| format!("reading the news from {}: {e}", route.label()))?;
    let days = texts_by_day(&texts);
    println!(
        "{} news texts over {} days from {} ({TEXT_KIND_NEWS}:{})",
        texts.len(),
        days.len(),
        route.label(),
        window.collector
    );

    for model in models {
        let dataset = text_score_dataset(model.name);
        let dir = imports.join(TEXT_SCORE_FORMAT).join(&dataset);
        let todo: Vec<_> = days.iter().filter(|(day, _)| !day_file(&dir, **day).exists()).collect();
        let (mut written, mut rows) = (0usize, 0usize);
        if !todo.is_empty() {
            let scorer = Scorer::load(model, models_dir).map_err(|e| e.to_string())?;
            let label = model.label();
            for (day, texts) in todo {
                let scored = score_rows(texts, &label, now_ms, |text| scorer.values(text))
                    .map_err(|e| e.to_string())?;
                if write_day_file(&dir, *day, &scored).map_err(|e| e.to_string())? {
                    written += 1;
                    rows += scored.len();
                }
            }
        }
        println!(
            "{}: {written} day files written ({rows} scores), {} kept, in {}",
            model.name,
            days.len() - written,
            dir.display()
        );
        println!(
            "  store them: vike-cli data history import {TEXT_SCORE_FORMAT} {dataset} --from {} --to {}",
            window.from, window.to
        );
    }
    Ok(())
}

/// This process's wall clock, epoch milliseconds UTC: the `scored_ms` stamp and the "has the day
/// ended" check. Read once per run, in the binary.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
