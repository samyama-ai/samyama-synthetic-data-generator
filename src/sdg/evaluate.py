"""sdg-eval: is the synthetic data equivalent to the real data, and is it safe?

Inputs are three all-categorical CSVs with identical columns:
  train    the real rows the generator was fitted on (members)
  holdout  real rows the fitter never saw (non-members; also the ML test set)
  synth    the generated rows

Every report carries two controls, because a metric without one proves nothing:
  leaky  bootstrap resample of `train`  -> the privacy audit MUST flag it
  naive  independent resample of 1-way marginals -> the utility metrics MUST show the loss

The empirical audit catches memorisation. It is NOT a proof of privacy; the (eps, delta)
carried in the fit ledger is the guarantee and is printed next to, never replaced by, it.
Needs the `eval` extra: numpy, scipy, scikit-learn.
"""
from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path

import numpy as np

DEFAULT_THRESHOLDS = {
    "tstr_trtr_auc_ratio_min": 0.90,     # utility: synth-trained AUC >= 90% of real-trained
    "mia_auc_synth_max": 0.55,           # privacy: synthetic must not be a membership oracle
    "mia_auc_leaky_min": 0.60,           # audit validity: bootstrap control must be flagged
    "exact_match_excess_max": 0.01,      # synth->train exact match <= holdout->train + 1pt
    "naive_must_trail_synth_by": 0.02,   # utility validity: naive control worse by >= 2pt AUC
}


def read_csv(path, columns=None):
    with open(path, newline="") as f:
        r = csv.DictReader(f)
        cols = columns or [c for c in r.fieldnames if c != "id"]
        rows = [[row[c] for c in cols] for row in r]
    return cols, rows


def encode(rows, domains, cols):
    idx = [{v: i for i, v in enumerate(domains[c])} for c in cols]
    try:
        return np.array([[idx[j][v] for j, v in enumerate(r)] for r in rows], dtype=np.int32)
    except KeyError as e:  # a synthetic value outside the public domain is itself a bug
        raise SystemExit(f"value {e} outside public domain")


def onehot(X, cards):
    from scipy import sparse
    n, d = X.shape
    offs = np.concatenate([[0], np.cumsum(cards)[:-1]])
    cols = (X + offs).ravel()
    rows = np.repeat(np.arange(n), d)
    return sparse.csr_matrix((np.ones(n * d, dtype=np.float32), (rows, cols)), shape=(n, int(sum(cards))))


# ---------------------------------------------------------------- fidelity
def tv_1way(A, B, cards):
    out = []
    for j, k in enumerate(cards):
        pa = np.bincount(A[:, j], minlength=k) / len(A)
        pb = np.bincount(B[:, j], minlength=k) / len(B)
        out.append(0.5 * np.abs(pa - pb).sum())
    return np.array(out)


def cramers_v(x, y, kx, ky):
    n = len(x)
    t = np.zeros((kx, ky))
    np.add.at(t, (x, y), 1)
    t = t[t.sum(1) > 0][:, t.sum(0) > 0]
    if min(t.shape) < 2:
        return 0.0
    e = np.outer(t.sum(1), t.sum(0)) / n
    chi2 = ((t - e) ** 2 / e).sum()
    return float(np.sqrt(chi2 / (n * (min(t.shape) - 1))))


def assoc_matrix(X, cards):
    d = X.shape[1]
    m = np.zeros((d, d))
    for i in range(d):
        for j in range(i + 1, d):
            m[i, j] = m[j, i] = cramers_v(X[:, i], X[:, j], cards[i], cards[j])
    return m


def fidelity(real, other, cards, label_idx):
    tv = tv_1way(real, other, cards)
    ar, ao = assoc_matrix(real, cards), assoc_matrix(other, cards)
    iu = np.triu_indices(len(cards), 1)
    diff = np.abs(ar - ao)[iu]
    lab = [abs(ar[label_idx, j] - ao[label_idx, j]) for j in range(len(cards)) if j != label_idx]
    return {
        "tv_1way_mean": float(tv.mean()), "tv_1way_max": float(tv.max()),
        "assoc_2way_mean_abs_diff": float(diff.mean()), "assoc_2way_max_abs_diff": float(diff.max()),
        "assoc_with_label_mean_abs_diff": float(np.mean(lab)),
    }


def ml_utility(train_X, test_X, cards, label_idx, seed):
    from sklearn.ensemble import RandomForestClassifier
    from sklearn.metrics import accuracy_score, roc_auc_score
    feat = [j for j in range(len(cards)) if j != label_idx]
    fc = [cards[j] for j in feat]
    Xtr, ytr = onehot(train_X[:, feat], fc), train_X[:, label_idx]
    Xte, yte = onehot(test_X[:, feat], fc), test_X[:, label_idx]
    clf = RandomForestClassifier(n_estimators=200, min_samples_leaf=5, random_state=seed, n_jobs=-1)
    clf.fit(Xtr, ytr)
    p = clf.predict_proba(Xte)[:, 1]
    return {"accuracy": float(accuracy_score(yte, p > 0.5)), "auc": float(roc_auc_score(yte, p))}


# ----------------------------------------------------------------- privacy
def min_hamming(Q, R, cards, chunk=2000):
    """Per-row minimum Hamming distance from each row of Q to the set R (0 = exact copy)."""
    d = Q.shape[1]
    OQ, OR = onehot(Q, cards), onehot(R, cards).T.tocsc()
    out = np.empty(len(Q), dtype=np.int32)
    for s in range(0, len(Q), chunk):
        m = (OQ[s:s + chunk] @ OR).toarray()
        out[s:s + chunk] = d - m.max(axis=1).astype(np.int32)
    return out


def auc_from_scores(pos, neg):
    from sklearn.metrics import roc_auc_score
    y = np.r_[np.ones(len(pos)), np.zeros(len(neg))]
    return float(roc_auc_score(y, np.r_[pos, neg]))


def privacy_audit(train, holdout, synth, cards, seed):
    rng = np.random.default_rng(seed)
    members = train[rng.choice(len(train), size=min(len(holdout), len(train)), replace=False)]
    d_mem = min_hamming(members, synth, cards)
    d_non = min_hamming(holdout, synth, cards)
    d_syn_train = min_hamming(synth, train, cards)
    d_hold_train = min_hamming(holdout, train, cards)
    return {
        # membership inference: attacker scores a candidate by closeness to the synthetic set
        "mia_auc": auc_from_scores(-d_mem, -d_non),
        "exact_match_synth_to_train": float((d_syn_train == 0).mean()),
        "exact_match_holdout_to_train": float((d_hold_train == 0).mean()),
        "dcr_mean_synth_to_train": float(d_syn_train.mean()),
        "dcr_mean_holdout_to_train": float(d_hold_train.mean()),
    }


# ------------------------------------------------------------------ report
def make_controls(train, cards, n, seed):
    rng = np.random.default_rng(seed)
    leaky = train[rng.integers(0, len(train), size=n)]
    naive = np.stack(
        [rng.choice(k, size=n, p=np.bincount(train[:, j], minlength=k) / len(train))
         for j, k in enumerate(cards)], axis=1).astype(np.int32)
    return leaky, naive


def evaluate(train_csv, holdout_csv, synth_csv, domains_json, label, ledger=None,
             seed=20261113, thresholds=None):
    th = {**DEFAULT_THRESHOLDS, **(thresholds or {})}
    domains = json.loads(Path(domains_json).read_text())
    cols, tr = read_csv(train_csv)
    _, ho = read_csv(holdout_csv, cols)
    _, sy = read_csv(synth_csv, cols)
    cards = [len(domains[c]) for c in cols]
    T, H, S = (encode(r, domains, cols) for r in (tr, ho, sy))
    li = cols.index(label)
    leaky, naive = make_controls(T, cards, len(S), seed)

    sets = {"synthetic": S, "control_leaky_bootstrap": leaky, "control_naive_independent": naive}
    rep = {"rows": {"train": len(T), "holdout": len(H), "synthetic": len(S)},
           "label": label, "ledger": ledger, "seed": seed, "thresholds": th,
           "trtr": ml_utility(T, H, cards, li, seed), "sets": {}}
    for name, X in sets.items():
        rep["sets"][name] = {
            "fidelity": fidelity(T, X, cards, li),
            "tstr": ml_utility(X, H, cards, li, seed),
            "privacy": privacy_audit(T, H, X, cards, seed),
        }
    s, lk, nv = (rep["sets"][k] for k in sets)
    ratio = s["tstr"]["auc"] / rep["trtr"]["auc"]
    checks = {
        "utility: TSTR/TRTR AUC >= min": ratio >= th["tstr_trtr_auc_ratio_min"],
        "utility control: naive trails synthetic": nv["tstr"]["auc"] <= s["tstr"]["auc"] - th["naive_must_trail_synth_by"],
        "privacy: synthetic MIA AUC <= max": s["privacy"]["mia_auc"] <= th["mia_auc_synth_max"],
        "privacy: synthetic not closer to train than holdout is":
            s["privacy"]["exact_match_synth_to_train"] <= s["privacy"]["exact_match_holdout_to_train"] + th["exact_match_excess_max"],
        "audit validity: leaky control flagged (MIA AUC >= min)": lk["privacy"]["mia_auc"] >= th["mia_auc_leaky_min"],
    }
    rep["tstr_trtr_auc_ratio"] = ratio
    rep["checks"] = {k: bool(v) for k, v in checks.items()}
    rep["passed"] = all(checks.values())
    return rep


def to_markdown(rep):
    f = lambda x: f"{x:.3f}"
    L = ["# sdg-eval report", ""]
    led = rep.get("ledger")
    if led:
        L += [f"Formal guarantee (from fit ledger): **({led.get('epsilon_total')}, {led.get('delta')})-DP**, "
              f"synthesizer `{led.get('synthesizer')}`. The empirical audit below is a check on top, not a substitute.", ""]
    L += [f"Rows: train {rep['rows']['train']}, holdout {rep['rows']['holdout']}, synthetic {rep['rows']['synthetic']}. "
          f"Label `{rep['label']}`, seed {rep['seed']}.", "",
          f"Real-trained ceiling (TRTR): acc {f(rep['trtr']['accuracy'])}, AUC {f(rep['trtr']['auc'])}.", "",
          "| set | TSTR acc | TSTR AUC | 1-way TV mean | 2-way assoc diff | assoc-with-label diff | MIA AUC | exact-match to train | mean DCR |",
          "|---|---|---|---|---|---|---|---|---|"]
    for n, d in rep["sets"].items():
        L.append(f"| {n} | {f(d['tstr']['accuracy'])} | {f(d['tstr']['auc'])} | {f(d['fidelity']['tv_1way_mean'])} | "
                 f"{f(d['fidelity']['assoc_2way_mean_abs_diff'])} | {f(d['fidelity']['assoc_with_label_mean_abs_diff'])} | "
                 f"{f(d['privacy']['mia_auc'])} | {f(d['privacy']['exact_match_synth_to_train'])} | {f(d['privacy']['dcr_mean_synth_to_train'])} |")
    p = rep["sets"]["synthetic"]["privacy"]
    L += ["", f"Reference: fresh real rows (holdout) match a training row exactly {f(p['exact_match_holdout_to_train'])} of the time, "
              f"mean DCR {f(p['dcr_mean_holdout_to_train'])}. Synthetic data that is no closer than that is not copying.",
          f"TSTR/TRTR AUC ratio: **{f(rep['tstr_trtr_auc_ratio'])}**.", "", "## Checks", ""]
    L += [f"- {'PASS' if ok else 'FAIL'}: {k}" for k, ok in rep["checks"].items()]
    L += ["", "## Scope", "",
          "- Records are the *binned, categorical* encoding; equivalence is claimed at that level.",
          "- Dependency structure is a tree rooted at the label (pairwise only); higher-order interactions are not captured.",
          "- MIA/DCR detect memorisation, not every leakage channel. Privacy rests on the DP ledger.",
          f"- Overall: **{'PASS' if rep['passed'] else 'FAIL'}**"]
    return "\n".join(L) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(prog="sdg-eval", description=__doc__.split("\n")[0])
    ap.add_argument("--train", required=True)
    ap.add_argument("--holdout", required=True)
    ap.add_argument("--synth", required=True)
    ap.add_argument("--domains", required=True)
    ap.add_argument("--label", required=True)
    ap.add_argument("--ledger", help="ledger.json written at fit time; printed in the report")
    ap.add_argument("--seed", type=int, default=20261113)
    ap.add_argument("--out", required=True, help="output prefix; writes <out>.json and <out>.md")
    a = ap.parse_args(argv)
    ledger = json.loads(Path(a.ledger).read_text()) if a.ledger else None
    rep = evaluate(a.train, a.holdout, a.synth, a.domains, a.label, ledger, a.seed)
    Path(a.out + ".json").write_text(json.dumps(rep, indent=1))
    Path(a.out + ".md").write_text(to_markdown(rep))
    print(to_markdown(rep))
    return 0 if rep["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
