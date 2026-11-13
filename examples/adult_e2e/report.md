# sdg-eval report

Formal guarantee (from fit ledger): **(1.0, 1e-6)-DP**, synthesizer `sdg_zcdp_marginal`. The empirical audit below is a check on top, not a substitute.

Rows: train 32561, holdout 16281, synthetic 32561. Label `income`, seed 20261113.

Real-trained ceiling (TRTR): acc 0.855, AUC 0.908.

| set | TSTR acc | TSTR AUC | 1-way TV mean | 2-way assoc diff | assoc-with-label diff | MIA AUC | exact-match to train | mean DCR |
|---|---|---|---|---|---|---|---|---|
| synthetic | 0.797 | 0.882 | 0.005 | 0.076 | 0.006 | 0.503 | 0.113 | 1.475 |
| control_leaky_bootstrap | 0.854 | 0.907 | 0.003 | 0.003 | 0.003 | 0.680 | 1.000 | 0.000 |
| control_naive_independent | 0.764 | 0.524 | 0.004 | 0.142 | 0.258 | 0.501 | 0.065 | 1.704 |

Reference: fresh real rows (holdout) match a training row exactly 0.495 of the time, mean DCR 0.642. Synthetic data that is no closer than that is not copying.
TSTR/TRTR AUC ratio: **0.971**.

## Checks

- PASS: utility: TSTR/TRTR AUC >= min
- PASS: utility control: naive trails synthetic
- PASS: privacy: synthetic MIA AUC <= max
- PASS: privacy: synthetic not closer to train than holdout is
- PASS: audit validity: leaky control flagged (MIA AUC >= min)

## Scope

- Records are the *binned, categorical* encoding; equivalence is claimed at that level.
- Dependency structure is a tree rooted at the label (pairwise only); higher-order interactions are not captured.
- MIA/DCR detect memorisation, not every leakage channel. Privacy rests on the DP ledger.
- Overall: **PASS**
