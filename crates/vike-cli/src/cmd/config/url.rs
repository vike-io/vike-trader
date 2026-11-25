//! `vike-cli config url ls|set|unset|probes` — **the venue ADDRESS rows** (`venue_url`).
//!
//! Every address a venue is dialled at is a row of the settings database, of the tier `live` or `demo`
//! (`vike_secrets::venue_url`; the vocabulary, the seed and the one lookup are
//! `vike_venues::venue_urls`'s; the stored column and the `--json` key stay `type`, which the IBKR
//! gateway scripts parse). A moved address is a row change plus a restart, never a code change.
//!
//! - `ls` prints every row; an `operator` row whose release default differs says what the default
//!   is now. `--json` is the shape a deploy script reads (the IBKR gateway scripts).
//! - `set` writes ONE row as `source = operator`, after the shape check (rest and stream are
//!   `http(s)://`, ws is `ws(s)://`, plain `http://`/`ws://` only to a loopback host, a socket is
//!   `host:port`) and the demo guard (a demo `trade` or `user` address may not equal the venue's live
//!   one). A refusal exits 2 and writes nothing. It applies at the next restart.
//! - `unset` puts ONE row back to the release's seed, `source = default` (`config unset`'s word).
//! - `probes` prints what the gatewayhub's probe last found at each row (it probes at its start and
//!   once a day). ⚠ It dials nothing itself: only the gatewayhub opens a venue connection
//!   (decision 0164), and this binary links no TLS stack (the `light-consumers` lane). It writes
//!   nothing.
//!
//! Every write is journalled like `config set` (`file` = `venue_url`, the row's dotted key).

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::venue_url::{
    UrlKey, UrlSource, VenueUrlRow, read_venue_urls, refusal_of, reset_venue_url_in, seed_of,
    set_venue_url_in,
};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType};

use crate::exit::{CliError, CommandResult};

/// The program name the verbs are read under.
const BIN: &str = "vike-cli config url";

/// What "down" means, as the gatewayhub's probe judges it (`crates/vike-gatewayhub/src/url_probe.rs`).
const DOWN_WORDS: &str = "a resource (a .jnlp) not answering 2xx/3xx, or a host with no DNS, TCP \
                          connect or TLS handshake";

/// The four verbs.
#[derive(clap::Subcommand, Debug)]
pub(crate) enum UrlVerb {
    #[command(about = "Every venue address row; an operator row also shows the release default")]
    Ls(LsCommand),
    #[command(about = "Write ONE address row (source = operator); it applies at the next restart")]
    Set(SetCommand),
    #[command(about = "Put ONE address row back to the release's default (source = default)")]
    Unset(UnsetCommand),
    #[command(about = "What the gatewayhub's probe last found at each address; dials nothing")]
    Probes(ProbesCommand),
}

/// `config url ls`'s arguments.
#[derive(clap::Args, Debug)]
pub(crate) struct LsCommand {
    #[arg(long, help = "the rows as one JSON array, for scripts")]
    json: bool,
    #[arg(long, value_name = "VENUE", help = "only this venue's rows")]
    venue: Option<String>,
}

/// `config url set`'s arguments.
#[derive(clap::Args, Debug)]
pub(crate) struct SetCommand {
    #[arg(help = "the venue (`binance`) or history vendor (`tardis`)")]
    venue: String,
    #[arg(value_name = "TIER", help = "live | demo")]
    tier: String,
    #[arg(help = "all | trade | user | data | history | login")]
    channel: String,
    #[arg(help = "rest | ws | socket | stream")]
    protocol: String,
    #[arg(help = "the BASE address: scheme://host[:port][/base], or host:port for a socket")]
    url: String,
    #[arg(long, value_name = "P", default_value = "", help = "the venue's book or plane word")]
    product: String,
    #[arg(long, value_name = "TEXT", help = "why, for a human (kept when omitted)")]
    note: Option<String>,
}

/// `config url unset`'s arguments.
#[derive(clap::Args, Debug)]
pub(crate) struct UnsetCommand {
    #[arg(help = "the venue (`binance`) or history vendor (`tardis`)")]
    venue: String,
    #[arg(value_name = "TIER", help = "live | demo")]
    tier: String,
    #[arg(help = "all | trade | user | data | history | login")]
    channel: String,
    #[arg(help = "rest | ws | socket | stream")]
    protocol: String,
    #[arg(long, value_name = "P", default_value = "", help = "the venue's book or plane word")]
    product: String,
}

/// `config url probes`'s arguments.
#[derive(clap::Args, Debug)]
pub(crate) struct ProbesCommand {
    #[arg(long, value_name = "VENUE", help = "only this venue's rows")]
    venue: Option<String>,
}

/// What a write needs from the dispatcher.
#[derive(Clone, Copy)]
pub(crate) struct Context<'a> {
    /// The settings directory the boot resolved.
    pub settings_dir: Option<&'a Path>,
    /// The instant a journal record is stamped with.
    pub now_ms: i64,
}

/// Entry point `config url` routes to.
pub(crate) fn run(verb: UrlVerb, ctx: Context<'_>) -> ExitCode {
    let (name, result) = match verb {
        UrlVerb::Ls(cmd) => ("ls", ls(&cmd, ctx.settings_dir)),
        UrlVerb::Set(cmd) => ("set", set(&cmd, ctx)),
        UrlVerb::Unset(cmd) => ("unset", unset(&cmd, ctx)),
        UrlVerb::Probes(cmd) => ("probes", probes(&cmd, ctx)),
    };
    match result {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{BIN} {name}: {}", e.message);
            e.exit.into()
        }
    }
}

/// Every row: the store's, or the release's seed where there is no store or no table yet.
fn rows(settings_dir: Option<&Path>) -> CommandResult<Vec<VenueUrlRow>> {
    match settings_dir {
        Some(dir) => read_venue_urls(dir).map_err(|e| CliError::failed(e.to_string())),
        None => Ok(vike_secrets::venue_url::seed_rows()),
    }
}

/// `venue tier channel protocol [product]`, the way an operator types it back.
fn spelled(k: &UrlKey) -> String {
    let product =
        if k.product.is_empty() { String::new() } else { format!(" --product {}", k.product) };
    format!("{} {} {} {}{product}", k.venue, k.ty, k.channel, k.protocol)
}

/// The release's default where an operator row differs from it.
fn release_default(r: &VenueUrlRow) -> Option<&'static str> {
    seed_of(&r.key).map(|s| s.url).filter(|url| r.source == UrlSource::Operator && *url != r.url)
}

fn ls(cmd: &LsCommand, settings_dir: Option<&Path>) -> CommandResult<Vec<String>> {
    let rows: Vec<VenueUrlRow> = rows(settings_dir)?
        .into_iter()
        .filter(|r| cmd.venue.as_deref().is_none_or(|v| r.key.venue == v))
        .collect();
    if cmd.json {
        let doc: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "venue": r.key.venue,
                    "type": r.key.ty.as_str(),
                    "channel": r.key.channel.as_str(),
                    "protocol": r.key.protocol.as_str(),
                    "product": r.key.product,
                    "url": r.url,
                    "source": r.source.as_str(),
                    "note": r.note,
                    "release_default": release_default(r),
                })
            })
            .collect();
        let text = serde_json::to_string_pretty(&doc).expect("strings and nulls serialize");
        return Ok(vec![text]);
    }
    let mut out = vec![format!("{} row(s); a change applies at the next restart", rows.len())];
    for r in &rows {
        let mut line = format!("{:<40}  {}  [{}]", spelled(&r.key), r.url, r.source.as_str());
        if let Some(default) = release_default(r) {
            line.push_str(&format!("  release default now: {default}"));
        }
        if !r.note.is_empty() {
            line.push_str(&format!("  ({})", r.note));
        }
        out.push(line);
    }
    Ok(out)
}

/// The key an operator typed, every word judged against the vocabulary.
fn key_of(
    venue: &str,
    tier: &str,
    channel: &str,
    protocol: &str,
    product: &str,
) -> CommandResult<UrlKey> {
    let known = vike_model::VENUES.contains(&venue)
        || vike_venues::venue_urls::VENUE_URL_SEED.iter().any(|s| s.venue == venue);
    if !known {
        return Err(CliError::usage(format!(
            "`{venue}` is neither a roster venue nor a history vendor with an address; nothing was \
             written. `vike-cli config url ls` lists every row"
        )));
    }
    Ok(UrlKey {
        venue: venue.to_string(),
        ty: tier.parse::<UrlType>().map_err(CliError::usage)?,
        channel: channel.parse::<UrlChannel>().map_err(CliError::usage)?,
        protocol: protocol.parse::<UrlProtocol>().map_err(CliError::usage)?,
        product: product.to_string(),
    })
}

fn journal(ctx: Context<'_>) -> vike_secrets::ChangeStamp {
    vike_secrets::ChangeStamp {
        actor: vike_change_journal::ChangeActor::cli("vike-cli"),
        process: vike_change_journal::ChangeProcess::current(env!("CARGO_PKG_VERSION")),
        now_ms: ctx.now_ms,
    }
}

fn dir_of(ctx: Context<'_>) -> CommandResult<&Path> {
    ctx.settings_dir.ok_or_else(|| {
        CliError::failed(
            "no settings directory resolved, so nothing can be written — `cd` into the project, \
             or name one with $VIKE_SETTINGS_DIR",
        )
    })
}

/// The report of one write, and its journal outcome said aloud.
fn written(
    key: &UrlKey,
    now: &str,
    outcome: Result<
        (Option<String>, Option<vike_secrets::ChangeAppendError>),
        vike_secrets::venue_url::VenueUrlRefusal,
    >,
) -> CommandResult<Vec<String>> {
    let dotted = key.dotted();
    let (previous, journal_error) = outcome.map_err(|refusal| {
        let (e, journal_error) = *refusal;
        if let Some(je) = journal_error {
            eprintln!(
                "{BIN}: ⚠ nothing was written, and the change journal could not record it: {je}"
            );
        }
        CliError::failed(e.to_string())
    })?;
    if let Some(e) = journal_error {
        eprintln!("{BIN}: ⚠ {dotted} WAS written, but the change journal could not record it: {e}");
    }
    Ok(vec![
        match previous {
            Some(old) => format!("{dotted} = {now} (was {old})"),
            None => format!("{dotted} = {now} (new row)"),
        },
        "  not live: it applies when the gatewayhub next restarts".to_string(),
    ])
}

fn set(cmd: &SetCommand, ctx: Context<'_>) -> CommandResult<Vec<String>> {
    let key = key_of(&cmd.venue, &cmd.tier, &cmd.channel, &cmd.protocol, &cmd.product)?;
    let url = cmd.url.trim();
    let dir = dir_of(ctx)?;
    let rows = rows(Some(dir))?;
    if let Some(why) = refusal_of(&rows, &key, url) {
        return Err(CliError::usage(format!("{}: {why} — nothing was written", key.dotted())));
    }
    written(&key, url, set_venue_url_in(dir, &key, url, cmd.note.as_deref(), journal(ctx)))
}

fn unset(cmd: &UnsetCommand, ctx: Context<'_>) -> CommandResult<Vec<String>> {
    let key = key_of(&cmd.venue, &cmd.tier, &cmd.channel, &cmd.protocol, &cmd.product)?;
    let dir = dir_of(ctx)?;
    let now = seed_of(&key).map_or("<no row: the release has no address for it>", |s| s.url);
    written(&key, now, reset_venue_url_in(dir, &key, journal(ctx)))
}

/// The table, printed as it is built; a failed row makes the exit a failure.
fn probes(cmd: &ProbesCommand, ctx: Context<'_>) -> CommandResult<Vec<String>> {
    let rows: Vec<VenueUrlRow> = rows(ctx.settings_dir)?
        .into_iter()
        .filter(|r| cmd.venue.as_deref().is_none_or(|v| r.key.venue == v))
        .collect();
    println!(
        "what the gatewayhub's probe last found (it probes at its start and once a day; this \
         command dials nothing). UNUSED: no active account dials the row, so it is never down. \
         LIMIT: a 429 or a timeout, rate-limited or temporarily refused rather than down"
    );
    let (mut down, mut limited) = (0, 0);
    for r in &rows {
        let (word, what) = match &r.probe {
            None => ("-", "not probed yet".to_string()),
            Some(p) => {
                let hours = (ctx.now_ms - p.at_ms).max(0) / vike_model::MS_PER_HOUR;
                let word = match p.ok {
                    Some(true) => "PASS",
                    Some(false) if refused_for_now(&p.words) => {
                        limited += 1;
                        "LIMIT"
                    }
                    Some(false) => {
                        down += 1;
                        "WARN"
                    }
                    None => "UNUSED",
                };
                (word, format!("{} ({hours} h ago)", p.words))
            }
        };
        println!("{word:<6} {:<40}  {}  {what}", spelled(&r.key), r.url);
    }
    match (down, limited) {
        (0, 0) => Ok(Vec::new()),
        (down, 0) => Err(CliError::failed(format!("{down} address(es) are down: {DOWN_WORDS}"))),
        (0, limited) => Err(CliError::failed(format!("{limited} address(es) are {LIMITED_WORDS}"))),
        (down, limited) => Err(CliError::failed(format!(
            "{down} address(es) are down: {DOWN_WORDS}; and {limited} {LIMITED_WORDS}"
        ))),
    }
}

/// **The `trade health` row**: the addresses IN USE whose last probe failed, as one WARN naming each
/// row, or a PASS when every probed row in use answered. A row no active account dials (the probe
/// recorded it not in use) is counted and never warns. `None` when no row has been probed (a box
/// whose gatewayhub has not run this build), so the verdict is not moved by a fact nobody measured.
pub(crate) fn health_check(settings_dir: Option<&Path>) -> Option<vike_client::health::Check> {
    use vike_client::health::{Check, Status};
    let rows = read_venue_urls(settings_dir?).ok()?;
    let probed: Vec<&VenueUrlRow> = rows.iter().filter(|r| r.probe.is_some()).collect();
    if probed.is_empty() {
        return None;
    }
    let unused = probed.iter().filter(|r| r.probe.as_ref().is_some_and(|p| p.ok.is_none())).count();
    let in_use = probed.len() - unused;
    let failed: Vec<(bool, String)> = probed
        .iter()
        .filter(|r| r.probe.as_ref().is_some_and(|p| p.ok == Some(false)))
        .map(|r| {
            let words = r.probe.as_ref().map_or("", |p| p.words.as_str());
            (refused_for_now(words), format!("{} {}: {words}", spelled(&r.key), r.url))
        })
        .collect();
    let limited = failed.iter().filter(|(refused, _)| *refused).count();
    let down = failed.len() - limited;
    let detail = match (down, limited) {
        (0, 0) => {
            let words = format!("{in_use} in use probed, every one answered; {unused} not in use");
            return Some(Check::new(Status::Pass, "venue addresses", words));
        }
        (down, 0) => format!("{down} of {in_use} in use are down ({DOWN_WORDS})"),
        (0, limited) => format!("{limited} of {in_use} in use are {LIMITED_WORDS}"),
        (down, limited) => format!(
            "{down} of {in_use} in use are down ({DOWN_WORDS}) and {limited} {LIMITED_WORDS}"
        ),
    };
    Some(
        Check::new(
            Status::Warn,
            "venue addresses",
            format!("{detail}; {unused} not in use; `vike-cli config url probes`"),
        )
        .items(failed.into_iter().map(|(_, line)| line).collect()),
    )
}

/// What a probe that met a `429` or a timeout means: the host refused this box FOR NOW (a venue's rate limit or a
/// temporary ban looks exactly so), which is not a dead endpoint.
const LIMITED_WORDS: &str = "rate-limited or temporarily refused (a 429 or a timeout: the venue may \
                             be pacing this box; not a dead endpoint)";

/// Whether a failed probe's words say the host refused for now rather than looked dead
/// (`vike_secrets::venue_url::PROBE_REFUSED_FOR_NOW`, which the gatewayhub's probe writes).
fn refused_for_now(words: &str) -> bool {
    words.starts_with(vike_secrets::venue_url::PROBE_REFUSED_FOR_NOW)
}

#[cfg(test)]
#[path = "url_tests.rs"]
mod tests;
