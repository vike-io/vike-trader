//! The human env-half printers: the registry table, the store's unmatched keys, and `wrap`.

use super::StoreStatus;
use super::resolve::{Resolved, Source, UnknownKeys};
use super::show_human::dash;

/// Greedy word-wrap to `width` columns. Hand-rolled because this crate adds no dependency for a
/// paragraph, and the input is prose from a `&'static str` table, never user data.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// The env half: the registry, resolved, plus the honest READS qualifier and its diagnosis.
pub(super) fn print_env_table(rows: &[Resolved], secrets: &StoreStatus, unknown: &UnknownKeys) {
    println!("-- environment variables --------------------------------------------------");
    // The middle rung is NAMED from the store that answered, never from a constant: this line read
    // `env > secrets.env > default` on every box, including the ones where that file had stopped
    // being read — a precedence claim about a store nothing consults.
    println!(
        "precedence: env > {} > default   (READS = what the reader consults)",
        secrets.label()
    );
    println!();
    if rows.is_empty() {
        println!("(no settings matched)");
        // …but a store key matching NO row is exactly the case a filter with no matching row can
        // still have found, so the complement is printed on this path too.
        print_unknown_store_keys(secrets, unknown);
        return;
    }

    let (mut wn, mut wv, mut wd, mut wk) =
        ("NAME".len(), "VALUE".len(), "DEFAULT".len(), "CRATE".len());
    for r in rows {
        wn = wn.max(r.name.len());
        wv = wv.max(dash(&r.value).len());
        wd = wd.max(dash(&r.default).len());
        wk = wk.max(r.krate.len());
    }
    // `database` is the longest source word; `caller-map` the longest reads word.
    let ws = "database".len().max("SOURCE".len());
    let wr = "caller-map".len().max("READS".len());

    println!(
        "{:<wn$}  {:<wv$}  {:<ws$}  {:<wr$}  {:<wd$}  {:<wk$}",
        "NAME", "VALUE", "SOURCE", "READS", "DEFAULT", "CRATE"
    );
    for r in rows {
        println!(
            "{:<wn$}  {:<wv$}  {:<ws$}  {:<wr$}  {:<wd$}  {:<wk$}",
            r.name,
            dash(&r.value),
            r.source.as_str(),
            r.reads.as_str(),
            dash(&r.default),
            r.krate
        );
    }
    println!();
    let changed = rows.iter().filter(|r| r.source != Source::Default).count();
    println!("{} setting(s) shown, {changed} configured (source != default)", rows.len());

    // The diagnosis the flat SOURCE word used to hide. Named rows, not a footnote: this is the
    // failure that presents as "the daemon ignores my key".
    let stranded: Vec<&Resolved> = rows.iter().filter(|r| r.store_may_not_reach_reader()).collect();
    if !stranded.is_empty() {
        println!();
        println!(
            "! {} row(s) take their value from {}, but that crate reads the variable \
             with a direct `env::var`:",
            stranded.len(),
            secrets.label()
        );
        for r in &stranded {
            println!("    {} ({})", r.name, r.krate);
        }
        println!(
            "  export them, or confirm that reader also falls back to the store — when one \
             variable is read"
        );
        println!(
            "  both ways the registry records only the DIRECT read, so this cannot tell the two \
             apart."
        );
    }

    // ⚠ THE OTHER DIAGNOSIS THE `READS` COLUMN CANNOT CARRY, and this command used to contradict
    // itself over it. `READS` says WHERE the read is; it says nothing about whether anything runs
    // that code. For a handful of flags nothing does — the read sits inside a poller no
    // composition root constructs — and the FILE half of this very command already prints
    // "NEITHER SPELLING CONFIGURES ANYTHING" for them. Printing those variables here as ordinary
    // rows under a header promising "READS = what the reader consults" was the same false positive
    // confirmation one section up, in the same invocation. The verdict is DERIVED
    // (`vike_config::env_verdict` over FLAG_REGISTRY x CONSUMPTION) so the two halves cannot drift;
    // a variable decision 0095 RETIRED gets none — exporting it refuses startup, which its registry
    // row's default already says.
    let dead: Vec<(&Resolved, &str)> =
        rows.iter().filter_map(|r| vike_config::env_verdict(r.name).map(|v| (r, v))).collect();
    if !dead.is_empty() {
        println!();
        println!(
            "! {} row(s) are read by code no shipped binary runs, so EXPORTING THEM CHANGES \
             NOTHING:",
            dead.len()
        );
        for (r, _) in &dead {
            println!("    {} ({})", r.name, r.krate);
        }
        println!(
            "  the feature is unmounted, not removed. `vike-cli config show --section file` \
             prints the"
        );
        println!("  per-key verdict and the reason; `--json` carries it as `reader_verdict`.");
    }

    print_unknown_store_keys(secrets, unknown);
}

/// The env table's COMPLEMENT: keys the store holds that no registry row covers. Silent when there
/// are none, so a block here always means something is genuinely unaccounted for.
///
/// See [`UnknownKeys`] for why the named/counted split is what it is.
fn print_unknown_store_keys(secrets: &StoreStatus, unknown: &UnknownKeys) {
    if unknown.is_empty() {
        return;
    }
    println!();
    // Named from the store the keys were actually READ OUT OF. The old spelling was a hardcoded
    // `settings/secrets.env`, which on a migrated box pointed the operator at a file that does not
    // hold the key it is complaining about.
    println!(
        "! {} holds key(s) that match NO row above — nothing else in this tool would",
        secrets.named()
    );
    println!("  show them, so a MIS-SPELLED variable name looks exactly like one you never set:");
    for key in &unknown.named {
        println!("    {key}");
    }
    if unknown.credential_shaped > 0 {
        println!(
            "    (+{} credential-shaped name(s), counted not named — this output is meant to be \
             pasted",
            unknown.credential_shaped
        );
        println!(
            "     into an issue. Most venue credential names are read through COMPUTED keys and \
             have no"
        );
        println!(
            "     registry row at all, so a non-zero count here is NORMAL and does not by itself \
             mean a"
        );
        println!("     typo. `vike-cli secrets list` names them.)");
    }
}
