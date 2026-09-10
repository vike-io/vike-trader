//! The `version=v4` text-model parser — and the crate's ONLY file read.
//!
//! # Why the text format and not `dump_model`'s JSON
//!
//! The text format is what `output_model=` writes and what `input_model=` reads back — the CLI's
//! own round trip, and what the C++ predictor itself treats as canonical. It was proven lossless
//! in the backend bakeoff: a model was loaded from disk, trained one more iteration, reserialized
//! (100 -> 101 trees, so the file was genuinely parsed and rewritten), and its predictions at
//! `num_iteration_predict=100` came back BYTE-IDENTICAL to the original's. The JSON dump has
//! historically TRAILED it — feature importances, feature infos and full tree detail were each
//! added later, in separate PRs — so the two are not guaranteed equivalent across versions.
//! Staying on the text format also keeps training and inference symmetric: `train` saves exactly
//! what this parses.
//!
//! Doubles are written with ~17 significant digits (`digits10 + 2`), so the format is a lossless
//! round trip of the in-memory model rather than an approximation of it.
//!
//! # Strictness, on purpose
//!
//! An unknown `version=` is [`MlError::UnsupportedVersion`], never a best-effort parse: LightGBM's
//! own issue tracker carries v3 models that fail to load in v4. An unknown objective, a multiclass
//! model and a linear tree are all refused for the same reason — each would produce a
//! plausible-looking number from the wrong arithmetic.
//!
//! The same rule is what makes this parser refuse things that are not malformed LINES at all. A
//! file cut at a tree boundary is a byte-exact prefix of a real model and every line in it is
//! valid, so [`parse_model_text`] requires the `end of trees` terminator and cross-checks the
//! header's `tree_sizes` count; a tree whose child indices leave the arrays or point backwards
//! parses line by line and only fails later, inside a live prediction, so `check_children`
//! refuses it here instead. Both are the same trade the version check makes: a refusal at load
//! time in place of a wrong number at prediction time.

use std::path::Path;
use std::str::FromStr;

use crate::error::MlError;
use crate::model::{GbdtModel, Objective, Tree};
use crate::pins::MODEL_VERSION;

/// One `key=value` line, with the 1-based line number it came from.
type Row = (usize, String, String);

fn find<'a>(rows: &'a [Row], key: &str) -> Option<&'a str> {
    rows.iter().find(|(_, k, _)| k == key).map(|(_, _, v)| v.as_str())
}

fn need<'a>(rows: &'a [Row], key: &str, at: usize) -> Result<&'a str, MlError> {
    find(rows, key).ok_or_else(|| MlError::Parse { line: at, what: format!("missing `{key}=`") })
}

fn scalar<T: FromStr>(rows: &[Row], key: &str, at: usize) -> Result<T, MlError> {
    let raw = need(rows, key, at)?;
    raw.trim()
        .parse::<T>()
        .map_err(|_| MlError::Parse { line: at, what: format!("`{key}={raw}` is not a number") })
}

/// Like [`scalar`], but an ABSENT key is `default` rather than an error. Distinct from a PRESENT
/// key that fails to parse, which is still a [`MlError::Parse`] — `unwrap_or(default)` on
/// [`scalar`]'s `Result` would swallow both cases identically, turning a genuinely malformed
/// `is_linear=` or `num_cat=` line into a silent default instead of a refusal.
fn scalar_or<T: FromStr>(rows: &[Row], key: &str, at: usize, default: T) -> Result<T, MlError> {
    match find(rows, key) {
        None => Ok(default),
        Some(raw) => raw.trim().parse::<T>().map_err(|_| MlError::Parse {
            line: at,
            what: format!("`{key}={raw}` is not a number"),
        }),
    }
}

/// A whitespace-separated array. An ABSENT key yields an empty vector, which is how a constant
/// tree's omitted split arrays are accepted; the length checks below then reject a genuinely
/// truncated array, so tolerance here costs no strictness there.
fn array<T: FromStr>(rows: &[Row], key: &str, at: usize) -> Result<Vec<T>, MlError> {
    let Some(raw) = find(rows, key) else { return Ok(Vec::new()) };
    raw.split_whitespace()
        .map(|tok| {
            tok.parse::<T>().map_err(|_| MlError::Parse {
                line: at,
                what: format!("`{key}` holds `{tok}`, which is not a number"),
            })
        })
        .collect()
}

/// `objective=binary sigmoid:1` -> [`Objective::Binary`].
///
/// The sigmoid lives HERE, in the header's objective line — it is not a per-tree field and not a
/// top-level `sigmoid=` key, and it is not always 1.0.
fn parse_objective(spec: &str, at: usize) -> Result<Objective, MlError> {
    let mut it = spec.split_whitespace();
    let name = it.next().unwrap_or_default();
    if name != "binary" {
        return Err(MlError::Unsupported(format!(
            "objective `{name}` (line {at}): this walker implements `binary` only"
        )));
    }
    let mut sigmoid = 1.0f64;
    for tok in it {
        if let Some(v) = tok.strip_prefix("sigmoid:") {
            sigmoid = v.parse().map_err(|_| MlError::Parse {
                line: at,
                what: format!("sigmoid `{v}` is not a number"),
            })?;
        }
    }
    if sigmoid <= 0.0 || sigmoid.is_nan() {
        return Err(MlError::Unsupported(format!(
            "sigmoid {sigmoid} must be > 0 — LightGBM's own binary objective refuses otherwise"
        )));
    }
    Ok(Objective::Binary { sigmoid })
}

/// Establish, ONCE, the two structural facts the walk then relies on without re-checking them.
///
/// [`crate::infer`]'s `leaf_value_for` indexes `decision_type[node]`, `left_child[node]` and
/// `leaf_value[!node]` raw, per row, in a live binary — the length checks above bound the arrays,
/// but nothing bounds the values INSIDE them. A child index past `n_internal` panics inside a
/// prediction; one pointing back at an already-visited node loops there forever. Both are
/// structurally impossible in a model LightGBM wrote, so both belong here, at load time, where the
/// answer is an [`MlError`] the caller can act on rather than a crash or a hang mid-tick.
///
/// ⚠ `child > node` is not a tidiness rule invented here — it is LightGBM's own node numbering.
/// `Tree::Split` appends each new internal node at index `num_leaves_ - 1` and rewrites the split
/// leaf's PARENT pointer to it, so a child's index always exceeds its parent's; the committed
/// 40-tree `real_binary_categorical_v4.txt` fixture satisfies it at every one of its nodes. It is
/// also exactly what makes the walk provably terminate: the node index strictly increases and is
/// bounded above by `n_internal`.
fn check_children(
    left_child: &[i32],
    right_child: &[i32],
    num_leaves: usize,
    at: usize,
) -> Result<(), MlError> {
    let n_internal = num_leaves - 1;
    for node in 0..n_internal {
        for (name, child) in [("left_child", left_child[node]), ("right_child", right_child[node])]
        {
            if child >= 0 {
                let target = child as usize;
                if target <= node || target >= n_internal {
                    return Err(MlError::Parse {
                        line: at,
                        what: format!(
                            "`{name}` entry {node} is {child}: an internal child must be an index \
                             in {}..{n_internal}. LightGBM numbers every child ABOVE its parent, \
                             so anything else is a walk that either leaves the arrays or never ends",
                            node + 1
                        ),
                    });
                }
            } else {
                let leaf = (!child) as usize;
                if leaf >= num_leaves {
                    return Err(MlError::Parse {
                        line: at,
                        what: format!(
                            "`{name}` entry {node} is {child}, i.e. leaf {leaf}, but this tree has \
                             {num_leaves} leaves — the walk would index past `leaf_value`"
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

fn parse_tree(rows: &[Row]) -> Result<Tree, MlError> {
    let at = rows.first().map(|(l, _, _)| *l).unwrap_or(0);
    if scalar_or::<u8>(rows, "is_linear", at, 0)? != 0 {
        return Err(MlError::Unsupported(
            "is_linear=1: linear trees carry per-leaf coefficients this walker does not evaluate"
                .to_string(),
        ));
    }
    let num_leaves: usize = scalar(rows, "num_leaves", at)?;
    if num_leaves == 0 {
        return Err(MlError::Parse { line: at, what: "num_leaves=0".into() });
    }
    let n_internal = num_leaves - 1;

    let split_feature: Vec<u32> = array(rows, "split_feature", at)?;
    let threshold: Vec<f64> = array(rows, "threshold", at)?;
    let decision_type: Vec<u8> = array(rows, "decision_type", at)?;
    let left_child: Vec<i32> = array(rows, "left_child", at)?;
    let right_child: Vec<i32> = array(rows, "right_child", at)?;
    let leaf_value: Vec<f64> = array(rows, "leaf_value", at)?;
    let cat_boundaries: Vec<u32> = array(rows, "cat_boundaries", at)?;
    let cat_threshold: Vec<u32> = array(rows, "cat_threshold", at)?;

    for (name, len) in [
        ("split_feature", split_feature.len()),
        ("threshold", threshold.len()),
        ("decision_type", decision_type.len()),
        ("left_child", left_child.len()),
        ("right_child", right_child.len()),
    ] {
        if len != n_internal {
            return Err(MlError::Parse {
                line: at,
                what: format!(
                    "`{name}` has {len} entries; num_leaves={num_leaves} needs {n_internal}"
                ),
            });
        }
    }
    if leaf_value.len() != num_leaves {
        return Err(MlError::Parse {
            line: at,
            what: format!("`leaf_value` has {} entries; num_leaves={num_leaves}", leaf_value.len()),
        });
    }
    let num_cat: usize = scalar_or(rows, "num_cat", at, 0)?;
    if num_cat > 0 && cat_boundaries.len() != num_cat + 1 {
        return Err(MlError::Parse {
            line: at,
            what: format!(
                "`cat_boundaries` has {} entries; num_cat={num_cat} needs {}",
                cat_boundaries.len(),
                num_cat + 1
            ),
        });
    }
    check_children(&left_child, &right_child, num_leaves, at)?;

    Ok(Tree {
        num_leaves,
        split_feature,
        threshold,
        decision_type,
        left_child,
        right_child,
        leaf_value,
        cat_boundaries,
        cat_threshold,
    })
}

/// Parse a LightGBM `save_model` text document.
pub fn parse_model_text(text: &str) -> Result<GbdtModel, MlError> {
    let mut header: Vec<Row> = Vec::new();
    let mut blocks: Vec<Vec<Row>> = Vec::new();
    let mut opened = false;
    let mut terminated = false;

    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let lineno = i + 1;
        if line.is_empty() {
            continue;
        }
        if !opened {
            if line != "tree" {
                return Err(MlError::Parse {
                    line: lineno,
                    what: format!("expected the model to open with `tree`, found `{line}`"),
                });
            }
            opened = true;
            continue;
        }
        // Everything below this marker is feature importances, the parameter dump and the Python
        // wrapper's `pandas_categorical` trailer — none of it is the model, and some of it is not
        // even `key=value`.
        if line == "end of trees" {
            terminated = true;
            break;
        }
        // `GBDT::SaveModelToString` writes this as a BARE line (no `=`) when `boosting=rf`:
        // `GBDT::Predict` then divides the summed score by `num_iteration_for_pred_`. This walker
        // only ever SUMS trees, so an averaged model would be silently wrong by a factor of
        // num_iterations if this fell through to the generic "not a key=value line" refusal below
        // — which the natural next robustness edit (tolerate unrecognised lines) would eventually
        // turn into no refusal at all. Named and refused here, ahead of that generic case.
        if line == "average_output" {
            return Err(MlError::Unsupported(
                "average_output (boosting=rf): this walker SUMS trees; an averaged model needs \
                 raw_score / num_iterations"
                    .into(),
            ));
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(MlError::Parse {
                line: lineno,
                what: format!("not a `key=value` line: `{line}`"),
            });
        };
        if k == "Tree" {
            blocks.push(Vec::new());
            continue;
        }
        let row = (lineno, k.to_string(), v.to_string());
        match blocks.last_mut() {
            Some(block) => block.push(row),
            None => header.push(row),
        }
    }
    if !opened {
        return Err(MlError::Parse { line: 0, what: "empty model text".into() });
    }
    // ⚠ The one check that catches a TRUNCATED file, and it has to be a check because truncation
    // is otherwise INVISIBLE here: a model cut at a tree boundary is a byte-exact prefix of a real
    // one, every line that remains is valid, and the result loads as a shorter, silently
    // under-boosted model that scores wrong with no error anywhere. `GBDT::SaveModelToString`
    // writes this line unconditionally after the last tree, so its absence is not a dialect — it
    // is a file that stops early (an interrupted copy, a full disk, a partial upload).
    if !terminated {
        return Err(MlError::Parse {
            line: text.lines().count(),
            what: "the model text ends without its `end of trees` line, so it is TRUNCATED. Every \
                   tree before the cut parses perfectly — accepting it would load a shorter model \
                   that predicts a plausible-looking wrong number"
                .into(),
        });
    }

    let version = need(&header, "version", 2)?;
    if version != MODEL_VERSION {
        return Err(MlError::UnsupportedVersion(version.to_string()));
    }
    let num_class: usize = scalar(&header, "num_class", 3)?;
    let per_iteration: usize = scalar(&header, "num_tree_per_iteration", 4)?;
    if num_class != 1 || per_iteration != 1 {
        return Err(MlError::Unsupported(format!(
            "num_class={num_class}, num_tree_per_iteration={per_iteration}: this walker implements \
             the single-output binary case only"
        )));
    }
    let max_feature_idx: usize = scalar(&header, "max_feature_idx", 6)?;
    let objective = parse_objective(need(&header, "objective", 7)?, 7)?;
    let feature_names: Vec<String> = find(&header, "feature_names")
        .unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect();

    let trees = blocks.iter().map(|b| parse_tree(b)).collect::<Result<Vec<_>, _>>()?;
    if trees.is_empty() {
        return Err(MlError::Parse { line: 0, what: "no `Tree=` block in the model text".into() });
    }
    // The header's OWN count of the trees, cross-checked against the ones actually here.
    // `tree_sizes=` carries one entry per tree (each block's byte length), so its LENGTH is
    // LightGBM's statement of how many trees this file holds — and it is the only thing that
    // catches a tree removed from the MIDDLE, which leaves the terminator above in place.
    //
    // Checked when PRESENT rather than required: every model LightGBM writes carries the line, but
    // this crate's own minimal hand-written test models legitimately do not, and a missing header
    // key is not what truncation looks like — a missing terminator is.
    if let Some((lineno, _, sizes)) = header.iter().find(|(_, k, _)| k == "tree_sizes") {
        let declared = sizes.split_whitespace().count();
        if declared != trees.len() {
            return Err(MlError::Parse {
                line: *lineno,
                what: format!(
                    "the header's `tree_sizes` declares {declared} trees but the file carries {}. \
                     A model missing trees still parses and still predicts — it is simply \
                     under-boosted, which is a wrong number rather than an error",
                    trees.len()
                ),
            });
        }
    }

    Ok(GbdtModel { max_feature_idx, objective, feature_names, trees })
}

/// Read and parse a model file the CALLER named.
///
/// This is the only I/O in the crate. Nothing here searches for a file, walks a directory, reads an
/// environment variable or has a default location: a library that guesses where its model lives is
/// a library its caller cannot deploy.
pub fn load_model_file(path: &Path) -> Result<GbdtModel, MlError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| MlError::Io(format!("{}: {e}", path.display())))?;
    parse_model_text(&text)
}
