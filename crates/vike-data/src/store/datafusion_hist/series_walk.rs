//! The store-tree walk: find every series leaf, and parse a leaf path back into its `SeriesId`.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::store::hist::DataError;
use crate::store::series::SeriesId;

use super::io;

/// Recursively collect series leaf dirs (those directly containing a `_manifest.json`) under `dir`.
/// A manifest marks a series leaf, whose only children are `date=` part dirs — so recursion stops
/// there (no nested series). Maintenance-only tree walk (drives [`DataFusionHist::list_series`]); not
/// the read path.
pub(crate) fn find_manifest_series_dirs(
    dir: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), DataError> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    let mut has_manifest = false;
    let mut subdirs = Vec::new();
    for entry in rd {
        let entry = entry.map_err(io)?;
        let ft = entry.file_type().map_err(io)?;
        if ft.is_dir() {
            subdirs.push(entry.path());
        } else if entry.path().file_name().and_then(|n| n.to_str()) == Some("_manifest.json") {
            has_manifest = true;
        }
    }
    if has_manifest {
        out.push(dir.to_path_buf());
        return Ok(()); // series leaf: children are date= part dirs, never nested series
    }
    for sub in subdirs {
        find_manifest_series_dirs(&sub, out)?;
    }
    Ok(())
}

/// Parse a series leaf dir back into its [`SeriesId`] by reading the `key=value` path segments below
/// `root`: `kind=…/venue=…[/source=…]/symbol=…[/interval=…]`. Returns `None` if `dir` isn't under `root`, if any
/// segment isn't one of the expected keys (so a stray dir can't masquerade as a series), or if the
/// `symbol=` segment is EMPTY (see the arm below — that value is the grouped-series sentinel, not a
/// symbol). The inverse of [`DataFusionHist::series_dir`].
pub(crate) fn parse_series_id(root: &Path, dir: &Path) -> Option<SeriesId> {
    let rel = dir.strip_prefix(root).ok()?;
    let (mut kind, mut venue, mut symbol, mut interval, mut group, mut source) =
        (None, None, None, None, None, None);
    for comp in rel.components() {
        let seg = comp.as_os_str().to_str()?;
        if let Some(v) = seg.strip_prefix("kind=") {
            kind = Some(v.to_string());
        } else if let Some(v) = seg.strip_prefix("venue=") {
            venue = Some(v.to_string());
        } else if let Some(v) = seg.strip_prefix("symbol=") {
            // An EMPTY `symbol=` value is REFUSED — the READ-side twin of `append_exec_orders`' and
            // `append_book_updates_grouped`'s writer guards, and general where those are per-producer:
            // a leaf can acquire this shape from any producer, a partial write or a hand-edit.
            //
            // `crates/vike-data/src/store/series.rs`'s `SeriesId::group` RESERVES an empty `symbol` as the
            // GROUPED-series sentinel ("exactly one of `symbol`/`group` is meaningful for any given
            // series"), so accepting it here minted a `SeriesId { symbol: "", group: None }` that is
            // NEITHER: no `scan_*(venue, symbol)` names it, [`SeriesId::label`] renders it as a BLANK
            // node in the Data Manager, `vike_studio_core::run`'s `bar_series`/`tick_series` offer it
            // as a runnable `(venue, "")` slice that scans nothing, and it rides the datahub wire to
            // remote clients in that state. The sentinel and the parser disagreed about the same
            // value and nothing noticed.
            //
            // `None` — the answer this function already gives an unexpected segment — rather than a
            // new error type: it has ONE caller (`list_series`'s `filter_map`), and a hard `Err`
            // there would let one malformed directory disable `list_series` for the WHOLE store,
            // taking `run_maintenance` and the Data Manager down with it. That is the opposite of
            // `run_maintenance`'s own rule that one broken series is one SKIPPED series.
            //
            // NOT re-read as GROUPED, which would be affirmatively wrong rather than merely
            // generous: the inverse `DataFusionHist::series_dir_of` would then rebuild `group=` — a
            // DIFFERENT, non-existent directory — so coverage/gaps/rebuild/compaction/retention/
            // delete would each silently operate on a phantom (the exact five-bug class
            // `series_dir_of`'s doc records), and the grouped read path tells rows apart by a
            // `symbol_col` column that a per-symbol part does not carry.
            //
            // Only `symbol`, mirroring #1307: an empty `venue=`/`kind=` collides with no sentinel,
            // and an empty `group=` still parses as GROUPED — degenerate-looking but UNAMBIGUOUS,
            // since `group.is_some()` is what every consumer tells the two layouts apart by.
            //
            // The `warn!` is load-bearing, not decoration. Skipping is a real behaviour change for
            // a leaf that already holds rows: `run_maintenance` stops compacting and
            // retention-pruning it, and the reconcile pre-seed — which iterates `list_series()` and
            // scans each `kind=exec_fill` series by `id.symbol` — stops folding its trade ids into
            // the seen-fill dedup set. That consumer is the sentinel doc's own "silently scanning
            // `\"\"`" case, so refusing is right; doing it QUIETLY would swap one silence for
            // another. (⚠ The pre-seed named here was `vike-app`'s and went with the GUI's local
            // core; the same walk is `crates/vike-core/src/journal_view.rs`'s
            // `journal_view_from_store` today, which no production root reaches yet.)
            if v.is_empty() {
                tracing::warn!(
                    dir = %dir.display(),
                    "list_series: SKIPPING a leaf whose `symbol=` segment is EMPTY — that value is \
                     the grouped-series sentinel, so the leaf names neither a symbol nor a group and \
                     no scan can address it. Nothing writes this shape today; move the rows under a \
                     real symbol, or delete the directory."
                );
                return None;
            }
            symbol = Some(v.to_string());
        } else if let Some(v) = seg.strip_prefix("interval=") {
            interval = Some(v.to_string());
        } else if let Some(v) = seg.strip_prefix("source=") {
            // ⚠ **THE ONE-WAY DOOR, and this arm is the door.** Before it existed this segment fell
            // into the `else` below, failed the `group=` strip and returned `None` for the WHOLE
            // path — so a build that predates this line, run against a store holding `source=`
            // leaves, silently omits them from `list_series`, and therefore from `inventory`, from
            // the Data Manager and from `run_maintenance`. Never compacted, never retention-pruned,
            // no error anywhere. That is not a bug to fix later: it is a property of every binary
            // already released, and no marker file helps because an old build does not read one.
            //
            // The owner ACCEPTED it on 2026-09-22 (the design's §6.3 and the Status block above
            // it), and the mitigation is SEQUENCING rather than code: this arm ships and is
            // RELEASED to every box BEFORE any producer is scoped, so the window between "a build
            // can read a sourced leaf" and "a store contains one" is as wide as the owner wants.
            // Its blast radius is bounded to series a NEW build created for a lane an OLD build
            // never knew about — legacy sourceless data stays fully visible to both, which is the
            // second argument for absence-as-a-value.
            //
            // ⚠ This parser is ORDER-AGNOSTIC (it matches prefixes in whatever order the path
            // yields them), so it does NOT enforce that `source=` sits under `venue=` and above
            // `symbol=`/`group=`. The BUILDERS decide that, and
            // `crates/vike-data/src/store/datafusion_hist.rs`'s `series_dir` carries why the position is
            // forced; `crates/vike-data/tests/store/source_dimension.rs`'s
            // `the_source_segment_sits_directly_under_venue` is the pin.
            source = Some(v.to_string());
        } else {
            // Not `group=` either, so this segment is unexpected and the leaf is not a
            // well-formed series — the `?` returns None for the whole path, exactly as the
            // explicit `return None` here did before 1.97's `question_mark` lint asked for it.
            let v = seg.strip_prefix("group=")?;
            group = Some(v.to_string());
        }
    }
    // A GROUPED leaf has no `symbol=` segment — it holds many symbols, told apart by the row-level
    // symbol column. Before this arm existed, `parse_series_id` returned None for such a dir, so
    // `list_series` silently SKIPPED grouped series and `run_maintenance` never compacted or
    // retention-pruned them. Latent rather than live (nothing wrote grouped series yet), and
    // exactly the kind of silent omission that only shows up as unbounded disk growth much later.
    if let Some(g) = group {
        let id = SeriesId::grouped(kind?, venue?, g);
        return Some(match source {
            Some(s) => id.with_source(s),
            None => id,
        });
    }
    Some(SeriesId { kind: kind?, venue: venue?, symbol: symbol?, interval, group: None, source })
}
