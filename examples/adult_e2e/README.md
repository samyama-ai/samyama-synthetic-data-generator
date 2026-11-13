# End-to-end: dataset → fit → synthetic data → "equivalent, and safe?"

A runnable demonstration on a **public** dataset (UCI Adult, CC BY 4.0). The method is the
same for a private dataset; the difference is that the raw rows never leave the owner.

```
raw rows ──fit (DP, inside the owner's boundary)──▶ adult.fitted.graphql + ledger.json
                                                       │   (noisy parameters only)
                                                       ▼
                                  sdg-rs schema  ──▶ synthetic rows ──▶ sdg-eval ──▶ report.md
```

## Run it

```bash
pip install '.[eval]'        # numpy, scipy, scikit-learn
examples/adult_e2e/run.sh    # ~3 min; writes examples/adult_e2e/report.md
```

`run.sh` downloads Adult (not vendored), encodes it, generates data from the **committed**
spec, and evaluates it. The fitting step is separate (`refit_private.sh`, not part of this
public tree): the spec and its privacy ledger are committed so everything downstream is
reproducible without it.

## What "equivalent" and "safe" mean here

| Question | Measure |
|---|---|
| Same marginals? | 1-way total variation per column |
| Same dependencies? | mean abs. difference of pairwise Cramér's V, and V against the label |
| Same *usefulness*? | train a classifier on synthetic, test on real rows the fitter never saw (TSTR), vs train on real (TRTR) |
| Copying records? | exact-match rate and distance-to-closest-record, synthetic→train, against the same numbers for fresh real rows |
| Membership leak? | membership-inference AUC (≈0.5 means no signal) |
| Formal guarantee | the (ε, δ) in `ledger.json`: zCDP Gaussian marginals, one row per entity |

Two **controls** are in every report, because a metric with no control proves nothing:
a *leaky* bootstrap of the real rows (the audit must flag it) and a *naive* independent-marginals
resample (the utility metrics must show the loss). Pass/fail thresholds were fixed before the
first run and live in `src/sdg/evaluate.py`.

## Result (ε = 1, δ = 1e-6; see `report.md`)

Synthetic TSTR AUC 0.882 vs real-trained 0.908 (ratio 0.97); the naive control reaches 0.524.
Membership-inference AUC 0.503; the leaky control 0.680. Synthetic rows are *farther* from the
training set (mean DCR 1.48) than fresh real rows are (0.64).

## Limits, stated plainly

- The comparison is on the **binned, categorical** encoding (fixed public cut points), not raw numerics.
- Structure is a **tree rooted at the label**: pairwise dependencies with the label are kept
  (association gap 0.006); feature–feature dependencies are only partly kept (mean pairwise gap
  0.076, vs 0.142 for independent columns). Higher-order interactions are not modelled.
- Single table. Foreign-key structure is the compiler's strength but is **not** exercised here.
- MIA/DCR detect memorisation, not every leakage channel. Differential privacy is the guarantee;
  the audit is a check on top. Neither makes data "privacy proof".
- Category domains are hard-coded public knowledge. On private data a data-derived domain would
  itself leak which rare values exist.
