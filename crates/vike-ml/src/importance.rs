//! Per-feature importance out of this crate's own `save_model` TEXT — gain and split count per
//! feature column.
//!
//! # Why this lives in vike-ml
//!
//! Everything this file knows is a fact about THIS crate. It parses the text
//! [`crate::train`] tells LightGBM to write and [`crate::parse`] reads back; it was verified
//! against the binary [`crate::pins`] pins; the column convention it documents is
//! [`crate::train::TrainData::write_csv`]'s (label first, no header, so the model's
//! `feature_names` are auto-generated and index→name mapping is the CALLER's job); and what it
//! reconstructs is precisely `split_gain`, one of the fields [`crate::model`] states it parses
//! past and drops because inference has no use for it. A consumer that owned this parser would be
//! documenting vike-ml's format from the outside, and would have to be re-verified against
//! vike-ml's binary every time either moved. It was written that way once — in the research crate,
//! which first wanted a feature-importance report and has since been dissolved outright — and this
//! is that file having come home.
//!
//! ⚠ Inference does not read any of this. [`crate::model`] drops `split_gain` deliberately, and
//! nothing here changes that: this is a SEPARATE fold over the same text, for a caller asking a
//! question about the fit rather than about a prediction.
//!
//! # The format, VERIFIED against the pinned binary — not remembered
//!
//! Trained with the pinned v4.7.0 binary (the CI box, `PROVENANCE` `tag=v4.7.0 sha=8f7036f0`,
//! 2026-08-11) on a CSV in the exact shape `crates/vike-ml/src/train.rs`'s `write_csv` produces
//! (label first, no header), the model text carries:
//!
//! * a header with `max_feature_idx=<N-1>` (the label column is NOT counted) and
//!   `feature_names=Column_0 Column_1 …` — AUTO-GENERATED names, because the training CSV this
//!   crate writes has NO header. The model file therefore cannot name a caller's columns;
//!   index→name mapping belongs to whoever packed the matrix, and the length equality against
//!   `max_feature_idx + 1` is the only honest check available here.
//! * one `Tree=<k>` section per tree, each with `split_feature=` (space-separated feature
//!   indices, one per internal node — `num_leaves - 1` of them) and `split_gain=`
//!   (space-separated floats, SAME count, pairwise: entry `j` of both describes the same split).
//! * a CONSTANT tree (`num_leaves=1`, LightGBM's `AsConstantTree` when a fit could not split)
//!   writes both keys with an EMPTY value — `split_feature=\n` — not an absent line. Verified on
//!   the same binary with a single-class label. (Hand-written minimal test models omit the lines
//!   entirely; both spellings mean zero splits and both are accepted.)
//! * `end of trees` terminates the tree block; per-tree lines never appear after it.
//!
//! # Why the parse REFUSES instead of shrugging
//!
//! A misaligned importance table is worse than none: it attributes the model's behaviour to the
//! wrong named feature and poisons every downstream pruning decision. So a `split_feature=` /
//! `split_gain=` pair whose counts disagree, an index at or past the declared width, an unparsable
//! entry, or a text with no `max_feature_idx=` is an `Err` naming the tree — never a row silently
//! skipped.
//!
//! # Why the error is a `String` and not [`crate::error::MlError`]
//!
//! Every message here names a TREE, and neither candidate variant frames it correctly:
//! `MlError::Parse` wants the 1-based LINE number this fold deliberately does not track (it reads
//! `key=value` payloads, not a line cursor), and `MlError::Shape` renders as `shape: …` for a
//! fault that is a PAIRING between two arrays rather than a row/feature count. The whole value of
//! these refusals is the sentence, so it is returned as one.

/// Per-feature importance of ONE fitted model, as plain data.
///
/// Indices are FEATURE column positions in the fitted matrix — the label column is not counted,
/// which is `crates/vike-ml/src/train.rs`'s `write_csv` convention and LightGBM's own — so
/// `gain[i]`/`splits[i]` describe the `i`-th packed column. The three lengths agree by contract:
/// `n_features == gain.len() == splits.len()`; a producer that cannot guarantee that must refuse
/// rather than hand back vectors a consumer would zip against the wrong names.
#[derive(Clone, Debug, PartialEq)]
pub struct FitImportance {
    /// The feature count the MODEL declares — not the caller's opinion of it. A caller compares
    /// this against the matrix it packed and refuses on mismatch, because a misaligned
    /// index→name mapping silently attributes importance to the wrong feature.
    pub n_features: usize,
    /// Total split gain accumulated per feature, `0.0` for a feature no tree ever split on.
    pub gain: Vec<f64>,
    /// How many splits used each feature, `0` for one never used.
    pub splits: Vec<u64>,
}

/// One `key=values` line's payload, if `line` is that key.
fn payload<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.strip_prefix(key).and_then(|rest| rest.strip_prefix('='))
}

/// Accumulate per-feature `(gain, splits)` over every tree of one model text.
///
/// [`FitImportance::n_features`] is the MODEL's own declaration (`max_feature_idx + 1`), so the
/// caller can compare it against the matrix it packed — the length check that makes a misaligned
/// mapping a refusal rather than a silently wrong table.
pub fn importance_from_model_text(text: &str) -> Result<FitImportance, String> {
    let mut n_features: Option<usize> = None;
    let mut in_trees = false;
    let mut tree: Option<usize> = None;
    let mut features: Option<Vec<usize>> = None;
    let mut gains: Option<Vec<f64>> = None;
    let mut gain: Vec<f64> = Vec::new();
    let mut splits: Vec<u64> = Vec::new();

    // Fold one tree's two arrays into the accumulators, refusing a half-present or mismatched
    // pair. Called at each tree boundary and at `end of trees`.
    let fold = |tree: &mut Option<usize>,
                features: &mut Option<Vec<usize>>,
                gains: &mut Option<Vec<f64>>,
                gain: &mut Vec<f64>,
                splits: &mut Vec<u64>|
     -> Result<(), String> {
        let Some(k) = tree.take() else { return Ok(()) };
        let f = features.take();
        let g = gains.take();
        match (f, g) {
            (Some(f), Some(g)) => {
                if f.len() != g.len() {
                    return Err(format!(
                        "Tree={k}: split_feature has {} entries but split_gain has {} — refusing \
                         to pair them",
                        f.len(),
                        g.len()
                    ));
                }
                for (i, v) in f.into_iter().zip(g) {
                    if i >= gain.len() {
                        return Err(format!(
                            "Tree={k}: split_feature names column {i} but the header declares \
                             only {} features (max_feature_idx + 1)",
                            gain.len()
                        ));
                    }
                    gain[i] += v;
                    splits[i] += 1;
                }
                Ok(())
            }
            // A constant tree in the minimal hand-written spelling: no split lines at all.
            // LightGBM's own constant tree writes both keys empty, which parses above as
            // `(Some([]), Some([]))`.
            (None, None) => Ok(()),
            (f, _) => Err(format!(
                "Tree={k}: {} without its partner — a doctored or truncated tree section",
                if f.is_some() { "split_feature=" } else { "split_gain=" }
            )),
        }
    };

    for line in text.lines() {
        let line = line.trim_end();
        if let Some(v) = payload(line, "max_feature_idx") {
            let idx: usize = v
                .trim()
                .parse()
                .map_err(|_| format!("max_feature_idx={v:?} is not a non-negative integer"))?;
            n_features = Some(idx + 1);
            gain = vec![0.0; idx + 1];
            splits = vec![0; idx + 1];
        } else if let Some(v) = payload(line, "Tree") {
            fold(&mut tree, &mut features, &mut gains, &mut gain, &mut splits)?;
            if n_features.is_none() {
                return Err("a Tree= section arrived before max_feature_idx= — the width every \
                            index must be checked against is missing"
                    .to_string());
            }
            tree = Some(v.trim().parse().map_err(|_| format!("Tree={v:?} is not a tree ordinal"))?);
            in_trees = true;
        } else if line == "end of trees" {
            fold(&mut tree, &mut features, &mut gains, &mut gain, &mut splits)?;
            in_trees = false;
        } else if in_trees {
            if let Some(v) = payload(line, "split_feature") {
                let k = tree.unwrap_or(0);
                if features.is_some() {
                    return Err(format!("Tree={k}: split_feature= appears twice"));
                }
                features = Some(
                    v.split_whitespace()
                        .map(|t| {
                            t.parse::<usize>().map_err(|_| {
                                format!("Tree={k}: split_feature entry {t:?} is not an index")
                            })
                        })
                        .collect::<Result<_, _>>()?,
                );
            } else if let Some(v) = payload(line, "split_gain") {
                let k = tree.unwrap_or(0);
                if gains.is_some() {
                    return Err(format!("Tree={k}: split_gain= appears twice"));
                }
                gains = Some(
                    v.split_whitespace()
                        .map(|t| {
                            t.parse::<f64>().map_err(|_| {
                                format!("Tree={k}: split_gain entry {t:?} is not a number")
                            })
                        })
                        .collect::<Result<_, _>>()?,
                );
            }
        }
    }
    // A text that never said `end of trees` still folds its last tree — `parse_model_text`
    // (which runs first on every real fit) is the authority on structural completeness; this
    // parser's own job ends at correct pairing.
    fold(&mut tree, &mut features, &mut gains, &mut gain, &mut splits)?;

    let n_features = n_features.ok_or_else(|| {
        "no max_feature_idx= line — this is not a LightGBM model text".to_string()
    })?;
    Ok(FitImportance { n_features, gain, splits })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-tree model in the VERIFIED v4.7.0 shape — header keys, per-tree keys and layout are
    /// copied from a real `save_model` output of the pinned binary (see the module doc), shrunk to
    /// the lines this parser reads plus the neighbours it must ignore.
    fn real_shape() -> String {
        "tree\n\
         version=v4\n\
         num_class=1\n\
         num_tree_per_iteration=1\n\
         label_index=0\n\
         max_feature_idx=3\n\
         objective=binary sigmoid:1\n\
         feature_names=Column_0 Column_1 Column_2 Column_3\n\
         feature_infos=[0:1] [0:1] [0:1] -1:0:1:2\n\
         tree_sizes=753 858\n\
         \n\
         Tree=0\n\
         num_leaves=6\n\
         num_cat=0\n\
         split_feature=0 0 1 1 0\n\
         split_gain=54.6525 11.031 18.3492 5.03417 2.32491\n\
         threshold=0.6 0.4 0.5 0.5 0.7\n\
         decision_type=2 2 2 2 2\n\
         left_child=1 -1 -3 4 -2\n\
         right_child=3 2 -4 -5 -6\n\
         leaf_value=-1 -0.8 -1 -0.7 -0.5 -0.6\n\
         is_linear=0\n\
         shrinkage=1\n\
         \n\
         Tree=1\n\
         num_leaves=3\n\
         num_cat=0\n\
         split_feature=3 0\n\
         split_gain=7.5 0.5\n\
         threshold=1.5 0.5\n\
         decision_type=1 2\n\
         left_child=1 -1\n\
         right_child=-2 -3\n\
         leaf_value=0.1 0.2 0.3\n\
         is_linear=0\n\
         shrinkage=0.1\n\
         \n\
         end of trees\n\
         \n\
         parameters:\n\
         [num_leaves: 7]\n\
         end of parameters\n"
            .to_string()
    }

    #[test]
    fn gains_and_split_counts_accumulate_per_feature_across_trees() {
        let imp = importance_from_model_text(&real_shape()).unwrap();
        assert_eq!(imp.n_features, 4, "max_feature_idx=3 declares four features");
        assert_eq!(imp.gain.len(), 4);
        assert_eq!(imp.splits.len(), 4);
        // Feature 0: three splits in Tree=0 (54.6525 + 11.031 + 2.32491) + one in Tree=1 (0.5).
        assert!((imp.gain[0] - (54.6525 + 11.031 + 2.32491 + 0.5)).abs() < 1e-9);
        assert_eq!(imp.splits[0], 4);
        // Feature 1: two splits, both in Tree=0.
        assert!((imp.gain[1] - (18.3492 + 5.03417)).abs() < 1e-9);
        assert_eq!(imp.splits[1], 2);
        // Feature 2: NEVER used — present in the table as an explicit zero, which is the whole
        // "which columns were never used" half of the report.
        assert_eq!(imp.gain[2], 0.0);
        assert_eq!(imp.splits[2], 0);
        // Feature 3 (the categorical): one split, in Tree=1.
        assert!((imp.gain[3] - 7.5).abs() < 1e-12);
        assert_eq!(imp.splits[3], 1);
    }

    #[test]
    fn a_constant_tree_with_empty_split_lines_counts_nothing() {
        // The VERIFIED degenerate shape: the pinned binary writes `split_feature=` and
        // `split_gain=` with an EMPTY payload for a `num_leaves=1` tree (see the module doc).
        let text = "tree\nversion=v4\nmax_feature_idx=1\n\
                    Tree=0\nnum_leaves=1\nsplit_feature=\nsplit_gain=\nleaf_value=0.5\n\
                    end of trees\n";
        let imp = importance_from_model_text(text).unwrap();
        assert_eq!(imp.n_features, 2);
        assert_eq!(imp.gain, vec![0.0, 0.0]);
        assert_eq!(imp.splits, vec![0, 0]);
    }

    #[test]
    fn a_constant_tree_with_no_split_lines_at_all_counts_nothing_too() {
        // The minimal hand-written model spelling (the shape a test double builds when there is no
        // trainer binary anywhere) omits the two lines entirely; both spellings mean the same
        // zero-split tree.
        let text = "tree\nversion=v4\nmax_feature_idx=0\n\
                    Tree=0\nnum_leaves=1\nleaf_value=0.75\nend of trees\n";
        let imp = importance_from_model_text(text).unwrap();
        assert_eq!(imp.n_features, 1);
        assert_eq!(imp.gain, vec![0.0]);
        assert_eq!(imp.splits, vec![0]);
    }

    #[test]
    fn mismatched_split_feature_and_split_gain_counts_are_refused_naming_the_tree() {
        let text = "tree\nmax_feature_idx=2\n\
                    Tree=0\nsplit_feature=0 1\nsplit_gain=5.0\nend of trees\n";
        let e = importance_from_model_text(text).unwrap_err();
        assert!(e.contains("Tree=0") && e.contains("2 entries") && e.contains('1'), "{e}");
    }

    #[test]
    fn an_index_at_or_past_the_declared_width_is_refused_rather_than_grown() {
        // ⚠ THE misattribution guard. Growing the vector would let a model trained on a wider
        // matrix than the caller packed produce a plausible table whose every row is shifted.
        let text = "tree\nmax_feature_idx=1\n\
                    Tree=0\nsplit_feature=2\nsplit_gain=5.0\nend of trees\n";
        let e = importance_from_model_text(text).unwrap_err();
        assert!(e.contains("column 2") && e.contains("2 features"), "{e}");
    }

    #[test]
    fn a_text_with_no_width_declaration_is_refused() {
        let e =
            importance_from_model_text("Tree=0\nsplit_feature=0\nsplit_gain=1.0\n").unwrap_err();
        assert!(e.contains("max_feature_idx"), "{e}");
        let e2 = importance_from_model_text("not a model at all").unwrap_err();
        assert!(e2.contains("max_feature_idx"), "{e2}");
    }

    #[test]
    fn a_lone_split_line_without_its_partner_is_refused() {
        let text = "tree\nmax_feature_idx=1\nTree=0\nsplit_feature=0\nend of trees\n";
        let e = importance_from_model_text(text).unwrap_err();
        assert!(e.contains("without its partner"), "{e}");
        let text = "tree\nmax_feature_idx=1\nTree=0\nsplit_gain=1.0\nend of trees\n";
        assert!(importance_from_model_text(text).is_err());
    }

    #[test]
    fn per_tree_keys_after_end_of_trees_are_ignored_as_lightgbm_writes_a_parameter_echo_there() {
        // The real file ends with a `parameters:` echo; nothing there may count as a split. The
        // fixture's echo carries `[num_leaves: 7]`, which starts with neither key, but a future
        // echo line starting `split_feature` must still not fold — hence the `in_trees` gate.
        let text = "tree\nmax_feature_idx=1\n\
                    Tree=0\nsplit_feature=0\nsplit_gain=2.0\nend of trees\n\
                    split_feature=1 1 1\nsplit_gain=9 9 9\n";
        let imp = importance_from_model_text(text).unwrap();
        assert_eq!(imp.splits, vec![1, 0], "a line after `end of trees` was folded");
        assert_eq!(imp.gain, vec![2.0, 0.0]);
    }

    #[test]
    fn an_unparsable_entry_is_refused_rather_than_skipped() {
        for bad in [
            "tree\nmax_feature_idx=1\nTree=0\nsplit_feature=x\nsplit_gain=1\nend of trees\n",
            "tree\nmax_feature_idx=1\nTree=0\nsplit_feature=0\nsplit_gain=abc\nend of trees\n",
            "tree\nmax_feature_idx=oops\n",
        ] {
            assert!(importance_from_model_text(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn a_prefix_key_is_not_mistaken_for_the_key_it_prefixes() {
        // `split_gain_something=` must not read as `split_gain=`; `payload` demands the `=`
        // directly after the key. Also `Tree=` vs `tree` (the file's first line) — the latter has
        // no `=` and is ignored.
        let text = "tree\nmax_feature_idx=1\n\
                    Tree=0\nsplit_feature=0\nsplit_featurex=9 9\nsplit_gain=1.5\n\
                    split_gainx=9 9\nend of trees\n";
        let imp = importance_from_model_text(text).unwrap();
        assert_eq!(imp.splits, vec![1, 0]);
    }
}
