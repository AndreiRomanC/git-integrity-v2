# Comparison limits and follow up

Recorded on 2026-10-25 at the user's request. Keep these comparison limitations separate from the general application review. This is a follow-up list, not authorization to implement further changes.

## Completed scope

The three agreed steps are implemented: consistent author and Shared filters with scoped counts and explanations; a separate Introduced by author analysis; and consistent List and Drill-down filtering with a compact header and matching ancestor containers. Opening an item preserves the full endpoint comparison.

The initial 2026-10-25 checkpoint passed 723 frontend tests and 472 Rust tests, with 5 Rust tests ignored. The frontend layout was inspected at widths from 800 to 1440 pixels. The changes still require a packaged build and validation on the company Windows workstation and real repository.

### Attribution clarification — 2026-10-26

Folder/gitlink conversions now keep structural evidence without counting their imported/removed descendants as individual file touches or Shared contributors. The root remains an atomic structural change; separate file edits still qualify. All differences retains structural explanations and Introduced by retains the conversion items. Detecting edits made inside the conversion commit itself remains inconclusive, not a claim that none occurred.

Surviving-contribution analysis now requires an explicit choice for the active author/comparison. Changing filter, author, folder or endpoints resets it; rerendering the same view does not. Evidence carries event kind, comparison side and the actual repository, including parent-pointer events. View change uses submodule details for pointer/conversion events rather than the child file viewer. Expanded-evidence logs identify the active mode, exact endpoints and witness, paths, side and anonymous repository ID; they exclude file contents and author identities.

Validation: 739 frontend tests and 493 Rust tests passed, 5 Rust tests ignored. The full Rust suite required loopback permission for four existing publish tests. New fixtures cover both conversion directions, later edits, distinct authors, reversed/divergent evidence and a 1,500-file conversion. Browser checks used the real frontend with synthetic data at 1440 and 1000 pixels; the filter toolbar remained 41 px tall across All/Introduced/Touched at 1440 px. No application release build or real company-repository validation was performed.

## Remaining limitations

1. **No automatic recursive submodule attribution.** Parent comparisons treat submodule pointers as atomic items. Open the direct submodule comparison to analyze internal authors. Exact child comparisons opened from a committed parent comparison can carry the immediate parent integration context; arbitrary child comparisons cannot invent that context.
2. **Strict attribution can be inconclusive.** Binary or NUL-encoded content, ambiguous origins, edited renames, changing context and size/history limits may prevent proof of a surviving contribution. Inconclusive must not mean that the selected author made no contribution.
3. **Large histories can exceed analysis budgets.** Surviving-contribution analysis has a 30-second limit; introduction analysis has a 60-second limit, plus bounded history/evidence limits. Request failures must not apply partial results. Measure these cases on Windows before changing budgets or performance behavior.
4. **Unavailable history and some structural transitions prevent introduction analysis.** Missing objects, shallow history, mismatched parent context and unsupported folder/gitlink conversions across the parent interval produce an explicit incomplete/error result. No automatic fetch occurs. This is not a verdict of no authorship.
5. **Merge-resolution authorship is intentionally conservative.** Files touched by author and Shared files use non-merge commits, so edits made exclusively during merge resolution do not qualify through the merge author. Introduced by author describes first-parent integration changes, not proof of personally written content.
6. **Identity is based on recorded Git authors.** Different email identities may represent the same person and can count as separate contributors in Shared files. Mailmap, co-author and squash/copy attribution are not implemented.

## Next verification

Use the original real comparison that produced the 151-item discrepancy. Check the same population in List and Drill-down, including path/search/state scoping and included/excluded counts. Inspect dated witness commits for `VWAQ4_B1.CDFX`, `Master_target.tdxml`, unrelated imported files, and folder/submodule conversions. Validate reversed comparisons, missing history, and direct submodule comparisons. Record elapsed times and any budget failures on Windows.

Do not label these limits as newly reproduced defects, and do not assume passing synthetic tests proves attribution for the company repository. Keep the comparison header compact and avoid recursive background scans or additional filters without a separate decision.

Implementation reference: [Author filtering in comparisons](../README.md#author-filtering-in-comparisons).
