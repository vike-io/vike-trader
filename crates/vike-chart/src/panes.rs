//! Identity-keyed, draggable sub-pane height allocation (chart-UX bundle T9).
//!
//! Before this module every sub-pane (Volume + one per visible oscillator)
//! shared ONE uniform clamp-derived height (`chart.rs`'s old `osc_h`/`price_h`
//! formula) — there was no per-pane memory and no user control. `PaneFractions`
//! replaces that with a persisted, identity-keyed *fraction of the available
//! height* per pane ([`PaneKey`]): known panes keep their fraction across
//! frames (and across symbol/timeframe/indicator churn, since the key is the
//! pane's kind + the oscillator's stable `Active::uid`, not its position in
//! the list); a never-before-seen pane gets a sane default share; a pane that
//! is temporarily hidden (e.g. Volume toggled off, or an oscillator removed
//! and re-added under the SAME uid is not applicable — but a re-toggled
//! Volume, or an oscillator whose visibility flips, is) keeps its stored
//! fraction untouched and re-slots at the same share when it reappears.
//!
//! [`PaneFractions::layout`] turns the current fraction map + the present
//! pane set into concrete pixel heights, normalized to fill `avail_px` with a
//! `min_px` floor per pane (degenerate `avail_px < panes.len() * min_px`
//! clamps every pane to `min_px` rather than producing negative/NaN heights —
//! the caller may then overflow visually, same as the old formula already
//! could). [`PaneFractions::drag`] is the separator-drag entry point: it
//! transfers height between exactly the two panes adjacent to the dragged
//! boundary, clamped so neither crosses `min_px`.
//!
//! Pure data + arithmetic — no egui here (that's `chart.rs`'s job: build
//! `present`, call `layout`/`drag`, draw the strips). Both derive
//! `Serialize`/`Deserialize` for T10's future persistence; T9 itself does not
//! wire persistence.

/// Identity of a chart sub-pane. `Study` is keyed by the oscillator's stable
/// `Active::uid` (assigned once at `WinState::add_indicator` time), NOT its
/// position in the visible list — so reordering/hiding/showing oscillators
/// never scrambles which stored fraction belongs to which pane.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum PaneKey {
    Price,
    Volume,
    // `alias = "Osc"`: workspace files written before the C1 `Osc`->`Study`
    // rename store this variant's externally-tagged JSON key as `"Osc"`. Without
    // the alias, deserializing an old `WinSnap::panes` entry fails the WHOLE
    // `Vec<(PaneKey, f32)>` (serde `default` only covers absent keys, not parse
    // errors), which bubbles up to reset the entire saved workspace. The alias
    // accepts both tags on read; serialization still always emits `"Study"`.
    #[serde(alias = "Osc")]
    Study(u64),
    /// CVD sub-pane (SP2) — a singleton pane like `Volume`, not per-uid like
    /// `Study`, since there is at most one CVD pane per chart.
    Cvd,
    /// C2b: a compare symbol moved out of the price-pane %-overlay into its
    /// OWN pane (mirrors `Study(u64)` — per-item, not a singleton — but keyed
    /// by `WinState::series_pane`'s compare-symbol identity rather than an
    /// indicator uid). No serde alias: this is a brand-new variant, not a
    /// renamed one, so pre-C2b saved workspaces simply never contain it.
    Series(u64),
}

/// Where to move a study, from a user's ••• "Move to" action (C1). Pure data —
/// no egui, no `WinState` coupling — so vike-chart can name it without a
/// vike-app dependency; `vike-app`'s `WinState::move_study` (C1 Task 2)
/// interprets it, and `chart::ChartActions` (C1 Task 3) carries it out of the
/// render closure back to the caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoveTarget {
    /// Allocate a fresh pane, inserted immediately ABOVE `anchor` in the
    /// authored pane order.
    NewAbove(PaneKey),
    /// Allocate a fresh pane, inserted immediately BELOW `anchor` in the
    /// authored pane order.
    NewBelow(PaneKey),
    /// Merge into the existing pane `PaneKey` (must already be present).
    Into(PaneKey),
}

/// Where a freshly-added indicator should land, chosen in the ƒx picker BEFORE the
/// add (indicator-target feature, part (a)). `Auto` reproduces the pre-feature
/// routing EXACTLY — overlay → price pane, oscillator → its own fresh study pane
/// (`WinState::add_indicator`'s default) — so a picker left untouched is
/// byte-identical to today. The other variants only take effect where the EXISTING
/// pane machinery (`WinState::move_study`/[`MoveTarget`]) can honor them; see
/// [`resolve_add_target`] for the exact support matrix. Pure data (no egui) so both
/// vike-chart and vike-app-core can name it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PaneTarget {
    /// Route by the registry's `RenderKind` — today's behavior (the default).
    #[default]
    Auto,
    /// Force onto the price (overlay) pane. Only meaningful for OVERLAY indicators
    /// (which already land there); an oscillator has no price-pane render path, so
    /// this falls back to its default fresh study pane.
    Price,
    /// A fresh study pane. Only meaningful for OSCILLATOR indicators (which already
    /// get one on add); an overlay has no own value-domain pane (out of scope), so
    /// it stays on the price pane.
    NewPane,
    /// Merge into an existing study pane (the same relocation the pane-header ⋯
    /// "Move to pane N" menu performs). Only meaningful for oscillators.
    Existing(PaneKey),
}

/// Resolve a picker [`PaneTarget`] for a JUST-ADDED indicator into an optional
/// relocation to apply via `WinState::move_study`. `kind` is the indicator's
/// registry `RenderKind`. Returns `None` whenever the placement
/// `WinState::add_indicator` already produced is correct — which is the case for:
///
/// - `Auto` (any indicator) — byte-identical to today;
/// - ANY overlay indicator — an overlay only ever renders on the price pane; an
///   overlay in its own value-domain pane is out of scope, so it is never relocated;
/// - an oscillator with `NewPane`/`Price`/`Auto` — `add_indicator` already gave it a
///   fresh study pane (and there is no oscillator→price-pane render path, so `Price`
///   falls back to that same fresh pane rather than stranding it).
///
/// Only an OSCILLATOR + [`PaneTarget::Existing`] yields `Some(MoveTarget::Into(..))`
/// — a merge into an existing study pane. `move_study`'s own `Into` guard drops a
/// stale/unknown pane, so a target that has since disappeared degrades to the
/// default fresh pane rather than rendering the study nowhere.
pub fn resolve_add_target(
    kind: crate::indicators::RenderKind,
    target: PaneTarget,
) -> Option<MoveTarget> {
    use crate::indicators::RenderKind;
    if kind == RenderKind::Overlay {
        return None; // overlays live on the price pane only (restriction: no own value-domain pane)
    }
    match target {
        PaneTarget::Existing(pane) => Some(MoveTarget::Into(pane)),
        // Auto / NewPane / Price all resolve to the fresh study pane `add_indicator`
        // already allocated for an oscillator — no relocation needed.
        PaneTarget::Auto | PaneTarget::NewPane | PaneTarget::Price => None,
    }
}

/// Default share (an unnormalized weight — see [`PaneFractions::layout`]) for
/// a pane seen for the first time. Price defaults to a majority share; Volume,
/// CVD (SP2), every oscillator, and every own-pane compare series (C2b)
/// default to an equal, smaller share. These are tuned so that with the
/// common case of ONE sub-pane the split lands close to the pre-T9 clamp
/// formula's typical result (price the clear majority, the sub-pane a
/// supporting strip) — see `task-9-report.md` for the worked comparison. With
/// N sub-panes each sub-pane's share divides further, which is a deliberate,
/// sensible difference from the old formula (which instead hard-capped every
/// sub-pane at 96px regardless of N, at price's expense) — not a
/// default-parity regression.
fn default_share(id: PaneKey) -> f32 {
    match id {
        PaneKey::Price => 0.60,
        PaneKey::Volume | PaneKey::Study(_) | PaneKey::Cvd | PaneKey::Series(_) => 0.16,
    }
}

/// Compose the frame's ordered pane list from the authored UNIFIED sub-pane
/// order (`sub_panes`) and the authored series-pane order (C2b Task 5).
/// `Price` is always first, then the unified sub-panes VERBATIM in the user's
/// arrangement, then the `series_panes` verbatim. This is the ONE place the
/// render's top-to-bottom order is defined — [`crate::chart::resolve_pane_layout`]
/// calls it (after gating `sub_panes` to those present this frame), then feeds
/// the result to [`PaneFractions::layout`] and the sub-pane dispatch loop.
///
/// Chart single-max default: `sub_panes` now carries Volume/CVD/Study as PEERS
/// in whatever top-to-bottom order the user arranged (previously the sequence
/// was hardcoded `Volume → Cvd → studies`). The caller passes an
/// already-present-filtered slice, so this fn is a pure prepend-Price +
/// concat-series and stays trivially unit-testable.
pub(crate) fn present_panes(sub_panes: &[PaneKey], series_panes: &[PaneKey]) -> Vec<PaneKey> {
    std::iter::once(PaneKey::Price)
        .chain(sub_panes.iter().copied())
        .chain(series_panes.iter().copied())
        .collect()
}

/// Persisted per-pane height fraction. Not a strict 0..=1 partition in
/// storage (entries for hidden panes are frozen, unnormalized weights left
/// over from whenever they were last visible) — [`layout`](Self::layout)
/// renormalizes across whatever is `present` on every call, so only the
/// RELATIVE weight between currently-present panes matters. A small `Vec` of
/// pairs rather than a `HashMap`: the pane count is tiny (price + a handful
/// of sub-panes), so linear lookup is cheap, and it keeps insertion order
/// stable for serde (T10's persistence) instead of a hasher's arbitrary order.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PaneFractions(Vec<(PaneKey, f32)>);

impl PaneFractions {
    fn get(&self, id: PaneKey) -> Option<f32> {
        self.0.iter().find(|(pid, _)| *pid == id).map(|(_, f)| *f)
    }

    fn set(&mut self, id: PaneKey, frac: f32) {
        match self.0.iter_mut().find(|(pid, _)| *pid == id) {
            Some(entry) => entry.1 = frac,
            None => self.0.push((id, frac)),
        }
    }

    fn weight_of(&self, id: PaneKey) -> f32 {
        self.get(id).unwrap_or_else(|| default_share(id)).max(f32::EPSILON)
    }

    /// Heights (px) for the CURRENT pane set, in `present`'s order. A
    /// never-before-seen id is PINNED to [`default_share`] in storage (so a
    /// later tuning change to that constant can't retroactively reshuffle an
    /// already-configured layout); an id already in storage — including one
    /// that was hidden (absent from `present`) on the previous call — is read
    /// as-is and left untouched here (only [`drag`](Self::drag) ever
    /// overwrites an existing entry). This is what makes a hide/re-show
    /// round-trip with nothing else changing reproduce the exact same split:
    /// if `layout` instead renormalized and wrote back every PRESENT pane's
    /// weight on every call, a still-visible pane's stored weight would drift
    /// whenever a sibling was temporarily hidden, corrupting its own
    /// eventual re-normalization once that sibling returned.
    ///
    /// The (possibly just-pinned) weights of the panes in `present` are
    /// normalized to sum to 1.0, then distributed over `avail_px` with each
    /// pane clamped to at least `min_px` (redistributing the deficit across
    /// the remaining flexible panes — see [`allocate`]). When `avail_px`
    /// can't fit every pane at `min_px`, every pane just gets `min_px` (the
    /// caller may overflow visually — same as the pre-T9 formula's own
    /// small-window edge case; never NaN/negative).
    pub fn layout(&mut self, present: &[PaneKey], avail_px: f32, min_px: f32) -> Vec<f32> {
        if present.is_empty() {
            return Vec::new();
        }
        for &id in present {
            if self.get(id).is_none() {
                self.set(id, default_share(id));
            }
        }
        let weights: Vec<f32> = present.iter().map(|&id| self.weight_of(id)).collect();
        let sum: f32 = weights.iter().sum();
        let fracs: Vec<f32> = if sum > f32::EPSILON {
            weights.iter().map(|w| w / sum).collect()
        } else {
            vec![1.0 / present.len() as f32; present.len()]
        };
        allocate(&fracs, avail_px, min_px)
    }

    /// Apply a separator drag between `present[i]` and `present[i + 1]`
    /// (`delta_px`: positive grows `present[i]` and shrinks `present[i + 1]`
    /// by the same amount — a separator dragged DOWN grows the pane above
    /// it). Re-derives the current on-screen heights via [`layout`](Self::layout)
    /// first (so the clamp below is relative to what's actually rendered,
    /// including any min-floor redistribution from OTHER panes), then moves
    /// exactly `delta_px` (clamped so neither neighbor drops below `min_px`)
    /// from one to the other.
    ///
    /// CRITICAL (T9 review fix): EVERY present pane is re-pinned to its
    /// current normalized height (`h_k / avail_px`), not just the dragged
    /// pair. Stored weights are raw defaults that never globally sum to 1.0
    /// (e.g. 0.60/0.16/0.16 → 0.92, by design — only their RELATIVE size
    /// matters, `layout` renormalizes per call). If `drag` rewrote only the
    /// pair on a sum-to-1 basis while the rest kept their un-normalized
    /// weights, the next `layout`'s renormalization would run over a
    /// mismatched sum and leak the delta onto NON-adjacent panes. Re-pinning
    /// all present panes to fractions that already sum to 1.0 makes that next
    /// renormalization an identity, so a drag moves EXACTLY the two adjacent
    /// panes. Only panes IN `present` are rewritten, so hidden-id retention
    /// is untouched (a toggled-off Volume keeps its stored weight). No-op on
    /// an out-of-range `i`, a single-pane set, or a non-positive `avail_px`.
    pub fn drag(
        &mut self,
        present: &[PaneKey],
        i: usize,
        delta_px: f32,
        avail_px: f32,
        min_px: f32,
    ) {
        if present.len() < 2 || i + 1 >= present.len() || avail_px <= f32::EPSILON {
            return;
        }
        let heights = self.layout(present, avail_px, min_px);
        let (h_i, h_j) = (heights[i], heights[i + 1]);
        // Neither neighbor may cross `min_px`. If one already sits AT/BELOW
        // min_px (a degenerate avail_px that couldn't fit everyone), the
        // valid range collapses to empty — freeze the drag rather than let
        // `f32::clamp` panic on `lo > hi`.
        let (lo, hi) = (min_px - h_i, h_j - min_px);
        if lo > hi {
            return;
        }
        let clamped = delta_px.clamp(lo, hi);
        if clamped == 0.0 {
            return;
        }
        // Re-pin ALL present panes to their current normalized heights so the
        // present-weight-sum is exactly 1.0 (see the CRITICAL note above),
        // then apply the ±delta to just the dragged pair.
        let d_frac = clamped / avail_px;
        for (&id, &h) in present.iter().zip(&heights) {
            self.set(id, (h / avail_px).max(0.0));
        }
        let id_i = present[i];
        let id_j = present[i + 1];
        self.set(id_i, (h_i / avail_px + d_frac).max(0.0));
        self.set(id_j, (h_j / avail_px - d_frac).max(0.0));
    }
}

/// Water-filling proportional allocation: distribute `avail_px` over
/// `fracs` (assumed to already sum to ~1.0), with every result clamped to
/// `>= min_px`. Panes that would fall below `min_px` at their raw share are
/// locked to exactly `min_px`; the freed-up budget is redistributed over the
/// remaining panes proportional to THEIR fracs, repeating until stable (at
/// most `fracs.len()` passes — each pass locks at least one more pane or
/// terminates). Returns `[min_px; n]` outright when `avail_px` can't fit
/// everyone (degenerate small window) instead of iterating to a
/// still-too-small fixed point.
fn allocate(fracs: &[f32], avail_px: f32, min_px: f32) -> Vec<f32> {
    let n = fracs.len();
    if n == 0 {
        return Vec::new();
    }
    let avail_px = avail_px.max(0.0);
    let min_px = min_px.max(0.0);
    if avail_px <= min_px * n as f32 {
        return vec![min_px; n];
    }
    let mut heights = vec![0.0_f32; n];
    let mut locked = vec![false; n];
    let mut remaining_px = avail_px;
    let mut remaining_w: f32 = fracs.iter().sum();
    loop {
        let free = n - locked.iter().filter(|&&l| l).count();
        if free == 0 {
            break;
        }
        let mut newly_locked = false;
        for i in 0..n {
            if locked[i] {
                continue;
            }
            let share = if remaining_w > 1e-9 {
                fracs[i] / remaining_w * remaining_px
            } else {
                remaining_px / free as f32
            };
            if share < min_px {
                heights[i] = min_px;
                locked[i] = true;
                remaining_px -= min_px;
                remaining_w -= fracs[i];
                newly_locked = true;
            }
        }
        if !newly_locked {
            break;
        }
    }
    let free = n - locked.iter().filter(|&&l| l).count();
    for i in 0..n {
        if !locked[i] {
            heights[i] = if remaining_w > 1e-9 {
                fracs[i] / remaining_w * remaining_px
            } else {
                remaining_px / free.max(1) as f32
            };
        }
    }
    heights
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: f32 = 44.0;

    #[test]
    fn pre_rename_osc_tag_still_deserializes_as_study() {
        // Backward file-compat: workspaces saved before the C1 Osc->Study rename
        // store the variant's externally-tagged key as "Osc". The serde alias must
        // still accept it (else the whole saved layout silently resets on load).
        let v: PaneKey = serde_json::from_str(r#"{"Osc":7}"#).unwrap();
        assert_eq!(v, PaneKey::Study(7));
        // and a full (PaneKey, f32) pane entry, as actually stored in WinSnap::panes
        let (k, frac): (PaneKey, f32) = serde_json::from_str(r#"[{"Osc":3},0.16]"#).unwrap();
        assert_eq!(k, PaneKey::Study(3));
        approx(frac, 0.16);
        // serialization always emits the new tag going forward
        assert_eq!(serde_json::to_string(&PaneKey::Study(7)).unwrap(), r#"{"Study":7}"#);
    }

    #[test]
    fn resolve_add_target_default_is_byte_identical() {
        use crate::indicators::RenderKind;
        // `Auto` never relocates, whatever the kind — the add-with-default guarantee.
        assert_eq!(resolve_add_target(RenderKind::Overlay, PaneTarget::Auto), None);
        assert_eq!(resolve_add_target(RenderKind::Oscillator, PaneTarget::Auto), None);
    }

    #[test]
    fn resolve_add_target_overlay_never_relocates() {
        use crate::indicators::RenderKind;
        // An overlay is restricted to the price pane: EVERY target is a no-op.
        for t in [
            PaneTarget::Auto,
            PaneTarget::Price,
            PaneTarget::NewPane,
            PaneTarget::Existing(PaneKey::Study(3)),
        ] {
            assert_eq!(resolve_add_target(RenderKind::Overlay, t), None, "overlay+{t:?}");
        }
    }

    #[test]
    fn resolve_add_target_oscillator_mapping() {
        use crate::indicators::RenderKind;
        // NewPane / Price both resolve to the fresh pane `add_indicator` already made.
        assert_eq!(resolve_add_target(RenderKind::Oscillator, PaneTarget::NewPane), None);
        assert_eq!(resolve_add_target(RenderKind::Oscillator, PaneTarget::Price), None);
        // Only Existing yields a real merge into that pane.
        assert_eq!(
            resolve_add_target(RenderKind::Oscillator, PaneTarget::Existing(PaneKey::Study(2))),
            Some(MoveTarget::Into(PaneKey::Study(2))),
        );
    }

    #[test]
    fn present_panes_prepends_price_then_sub_then_series() {
        use PaneKey::*;
        // Price is always first and always present, whatever the sub-panes are.
        assert_eq!(present_panes(&[], &[]), vec![Price]);
        assert_eq!(present_panes(&[Volume], &[]), vec![Price, Volume]);
        assert_eq!(present_panes(&[Cvd], &[]), vec![Price, Cvd]);
        assert_eq!(present_panes(&[Volume, Cvd], &[]), vec![Price, Volume, Cvd]);
        // Chart single-max default: Volume/CVD/Study are PEERS — the unified
        // sub-order is emitted VERBATIM (no forced Volume→Cvd→studies sequence),
        // so any user arrangement survives to the render.
        let sub = [Study(3), Volume, Study(1), Cvd, Study(2)];
        assert_eq!(
            present_panes(&sub, &[]),
            vec![Price, Study(3), Volume, Study(1), Cvd, Study(2)],
        );
        // C2b: authored series panes follow verbatim, AFTER the unified sub-panes.
        let series = [Series(5), Series(2)];
        assert_eq!(
            present_panes(&[Volume, Cvd, Study(3)], &series),
            vec![Price, Volume, Cvd, Study(3), Series(5), Series(2)],
        );
        // Series panes alone (no sub-panes): Price then series directly.
        assert_eq!(present_panes(&[], &series), vec![Price, Series(5), Series(2)]);
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 0.01, "{a} !~= {b}");
    }

    #[test]
    fn layout_normalizes_to_avail_at_defaults() {
        let mut pf = PaneFractions::default();
        let present = [PaneKey::Price, PaneKey::Volume, PaneKey::Study(1)];
        let heights = pf.layout(&present, 600.0, MIN);
        assert_eq!(heights.len(), 3);
        approx(heights.iter().sum(), 600.0);
        // price is the clear majority share
        assert!(heights[0] > heights[1]);
        assert!(heights[0] > heights[2]);
    }

    #[test]
    fn new_osc_gets_default_share_existing_scale_proportionally() {
        let mut pf = PaneFractions::default();
        let base = [PaneKey::Price, PaneKey::Volume];
        let before = pf.layout(&base, 600.0, MIN);
        let ratio_before = before[0] / before[1];

        let with_new = [PaneKey::Price, PaneKey::Volume, PaneKey::Study(7)];
        let after = pf.layout(&with_new, 600.0, MIN);
        assert_eq!(after.len(), 3);
        approx(after.iter().sum(), 600.0);
        // the new osc pane got a real (non-zero, non-dominant) share
        assert!(after[2] > MIN - 1.0);
        assert!(after[2] < after[0]);
        // price:volume ratio is preserved (both shrank together to make room)
        let ratio_after = after[0] / after[1];
        approx(ratio_before, ratio_after);
        // and both shrank in absolute terms vs. the 2-pane layout
        assert!(after[0] < before[0]);
        assert!(after[1] < before[1]);
    }

    #[test]
    fn hidden_pane_keeps_fraction_and_reslots() {
        let mut pf = PaneFractions::default();
        let with_vol = [PaneKey::Price, PaneKey::Volume];
        let shown = pf.layout(&with_vol, 600.0, MIN);
        let vol_frac_before = shown[1] / 600.0;

        // Volume toggled off: layout with just Price present.
        let price_only = [PaneKey::Price];
        let hidden = pf.layout(&price_only, 600.0, MIN);
        assert_eq!(hidden.len(), 1);
        approx(hidden[0], 600.0); // price alone takes the whole budget

        // Volume toggled back on: same fraction as before it was hidden.
        let reshown = pf.layout(&with_vol, 600.0, MIN);
        approx(reshown[1] / 600.0, vol_frac_before);
        approx(reshown[0], shown[0]); // price's share is unaffected by the round-trip
    }

    #[test]
    fn drag_moves_boundary_by_delta_and_respects_min() {
        let mut pf = PaneFractions::default();
        let present = [PaneKey::Price, PaneKey::Volume];
        let before = pf.layout(&present, 600.0, MIN);

        pf.drag(&present, 0, 20.0, 600.0, MIN);
        let after = pf.layout(&present, 600.0, MIN);
        approx(after[0], before[0] + 20.0);
        approx(after[1], before[1] - 20.0);
        approx(after.iter().sum(), 600.0);

        // an enormous drag clamps so pane 1 never crosses min
        pf.drag(&present, 0, 10_000.0, 600.0, MIN);
        let clamped = pf.layout(&present, 600.0, MIN);
        approx(clamped[1], MIN);
        assert!(clamped[0] >= MIN);
        approx(clamped[0] + clamped[1], 600.0);

        // and the opposite direction clamps pane 0
        pf.drag(&present, 0, -10_000.0, 600.0, MIN);
        let clamped2 = pf.layout(&present, 600.0, MIN);
        approx(clamped2[0], MIN);
        approx(clamped2[0] + clamped2[1], 600.0);
    }

    #[test]
    fn drag_moves_only_the_two_adjacent_panes_in_a_3_pane_layout() {
        // T9 review fix: with 3+ panes a drag must move EXACTLY the two panes
        // adjacent to the separator; a non-adjacent pane's height is invariant.
        // (Pre-fix this leaked the delta onto the non-adjacent pane because
        // `drag` re-pinned only the pair, leaving the present-weight-sum ≠ 1.0
        // for the next layout's renormalization.)
        let present = [PaneKey::Price, PaneKey::Volume, PaneKey::Study(1)];

        // separator 0 (Price↔Volume): Study(1) must not move.
        let mut pf = PaneFractions::default();
        let before = pf.layout(&present, 600.0, MIN);
        pf.drag(&present, 0, 20.0, 600.0, MIN);
        let after = pf.layout(&present, 600.0, MIN);
        approx(after[0], before[0] + 20.0); // Price grew by exactly +20
        approx(after[1], before[1] - 20.0); // Volume shrank by exactly -20
        approx(after[2], before[2]); // Study(1): non-adjacent, invariant
        approx(after.iter().sum(), 600.0);

        // separator 1 (Volume↔Study): Price must not move.
        let mut pf = PaneFractions::default();
        let before = pf.layout(&present, 600.0, MIN);
        pf.drag(&present, 1, 20.0, 600.0, MIN);
        let after = pf.layout(&present, 600.0, MIN);
        approx(after[0], before[0]); // Price: non-adjacent, invariant
        approx(after[1], before[1] + 20.0); // Volume grew by exactly +20
        approx(after[2], before[2] - 20.0); // Study shrank by exactly -20
        approx(after.iter().sum(), 600.0);

        // over-drag on separator 1 still clamps Study at min, and STILL leaves
        // the non-adjacent Price untouched.
        let mut pf = PaneFractions::default();
        let before = pf.layout(&present, 600.0, MIN);
        pf.drag(&present, 1, 10_000.0, 600.0, MIN);
        let clamped = pf.layout(&present, 600.0, MIN);
        approx(clamped[2], MIN); // Study floored at min
        assert!(clamped[1] >= MIN);
        approx(clamped[0], before[0]); // Price still invariant under the over-drag
        approx(clamped.iter().sum(), 600.0);
    }

    #[test]
    fn drag_out_of_range_is_a_no_op() {
        let mut pf = PaneFractions::default();
        let present = [PaneKey::Price];
        pf.drag(&present, 0, 50.0, 600.0, MIN); // single pane: no boundary 0
        assert!(pf.0.is_empty());

        let present2 = [PaneKey::Price, PaneKey::Volume];
        pf.drag(&present2, 5, 50.0, 600.0, MIN); // out-of-range i
        assert!(pf.0.is_empty());
    }

    #[test]
    fn degenerate_avail_clamps_sanely() {
        let mut pf = PaneFractions::default();
        let present = [PaneKey::Price, PaneKey::Volume, PaneKey::Study(1), PaneKey::Study(2)];
        // avail well under 4 * min — every pane floors to `min`, no NaN/negative/panic.
        let heights = pf.layout(&present, 100.0, MIN);
        assert_eq!(heights.len(), 4);
        for h in &heights {
            assert!(h.is_finite());
            approx(*h, MIN);
        }

        // zero and negative avail: still finite, non-negative, no panic.
        for bad in [0.0_f32, -50.0] {
            let heights = pf.layout(&present, bad, MIN);
            for h in &heights {
                assert!(h.is_finite());
                assert!(*h >= 0.0);
            }
        }

        // a drag in this fully-degenerate state is a safe no-op, not a panic.
        pf.drag(&present, 1, 500.0, 100.0, MIN);
        let still = pf.layout(&present, 100.0, MIN);
        for h in &still {
            approx(*h, MIN);
        }
    }
}
