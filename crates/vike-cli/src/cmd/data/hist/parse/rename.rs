//! `data history rename` — the arm of the per-subcommand `match` in `super` that holds its
//! grammar; see `fetch` for how the pieces fit.

use super::super::{Filter, RenameArgs};

/// `rename`'s arm: MOVES `--kind`/`--venue` out of `filter` (the selector here, as on `rm`) and builds
/// the `RenameArgs` — `--symbol` is the name stored now and `--to` the new one, both required.
pub(in crate::cmd::data::hist) fn parse(
    filter: &mut Filter,
    symbol: Option<String>,
    group: Option<String>,
    interval: Option<String>,
    lane: Option<String>,
    to: Option<String>,
    dry_run: bool,
    yes: bool,
) -> Result<RenameArgs, String> {
    if group.is_some() {
        return Err("--group does not apply to `rename`: it moves PER-SYMBOL series. A grouped \
             series holds many instruments in one part, told apart by each row's own symbol, so \
             renaming the group would rename none of them"
            .to_string());
    }
    let venue = filter.venue.take().ok_or(
        "rename needs --venue: an instrument's name is a name ON one venue, and a rename never \
         crosses venues",
    )?;
    let from = symbol.ok_or("rename needs --symbol: the name the series are stored under NOW")?;
    let to = to.ok_or("rename needs --to: the NEW name")?;
    // The new name as the store files it (decision 0290, «Б» simplified): a user name's builder-dex `:` is `-`.
    let to = vike_paths::store_path::store_name_of_user_name(&to);
    let kind = filter.kind.take();
    for (flag, value) in [
        ("--venue", Some(&venue)),
        ("--symbol", Some(&from)),
        ("--to", Some(&to)),
        ("--kind", kind.as_ref()),
        ("--interval", interval.as_ref()),
        ("--lane", lane.as_ref()),
    ] {
        let Some(value) = value else { continue };
        if value.trim().is_empty() {
            return Err(format!(
                "{flag} was given an EMPTY value: omit an optional flag to move every kind, \
                 interval and lane; a name is never blank"
            ));
        }
        if let Some(c) = value.chars().find(|c| "*?[".contains(*c)) {
            return Err(format!(
                "{flag} value {value:?} contains the glob character {c:?}. `rename` names one \
                 instrument exactly; there is no matcher here for a glob to feed"
            ));
        }
    }
    if from == to {
        return Err(format!("--symbol and --to are both {from:?}: there is nothing to rename"));
    }
    // The store's own write rule, refused here on the usage rung before a process is spawned; the
    // store tool applies it again, for a caller that reaches it directly.
    vike_paths::store_path::refuse_a_path_hostile_symbol(&to)
        .map_err(|why| format!("--to: {why}"))?;
    Ok(RenameArgs { venue, from, to, kind, interval, source: lane, dry_run, yes })
}
