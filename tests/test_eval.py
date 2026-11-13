"""sdg-eval must behave on its own controls: it is only trustworthy if the leaky control is
flagged and the naive control is penalised. Small synthetic world, runs in seconds."""
import csv
import json

import numpy as np
import pytest

pytest.importorskip("sklearn")
from sdg.evaluate import evaluate  # noqa: E402

D, K = 8, 6
COLS = [f"f{i}" for i in range(D)] + ["y"]


def world(n, rng):
    X = rng.integers(0, K, size=(n, D))
    # label depends on two features -> a dependency a naive independent model cannot keep
    p = 1 / (1 + np.exp(-(1.2 * (X[:, 0] - 2.5) + 0.9 * (X[:, 1] - 2.5))))
    y = (rng.random(n) < p).astype(int)
    return np.column_stack([X, y])


def write(path, M):
    with open(path, "w", newline="") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(COLS)
        for r in M:
            w.writerow([f"v{v}" for v in r])


def test_controls_behave_and_faithful_synthetic_passes(tmp_path):
    rng = np.random.default_rng(0)
    train, hold, synth = world(3000, rng), world(1500, rng), world(3000, rng)
    for n, M in [("train", train), ("hold", hold), ("synth", synth)]:
        write(tmp_path / f"{n}.csv", M)
    dom = {c: [f"v{i}" for i in range(K)] for c in COLS[:-1]}
    dom["y"] = ["v0", "v1"]
    (tmp_path / "dom.json").write_text(json.dumps(dom))
    rep = evaluate(tmp_path / "train.csv", tmp_path / "hold.csv", tmp_path / "synth.csv",
                   tmp_path / "dom.json", "y", seed=1)
    c = rep["checks"]
    assert c["audit validity: leaky control flagged (MIA AUC >= min)"], rep["sets"]["control_leaky_bootstrap"]["privacy"]
    assert c["utility control: naive trails synthetic"], rep["sets"]
    assert rep["sets"]["control_leaky_bootstrap"]["privacy"]["exact_match_synth_to_train"] == 1.0
    assert rep["passed"], rep["checks"]


def test_memorising_synthetic_is_rejected(tmp_path):
    rng = np.random.default_rng(1)
    train, hold = world(3000, rng), world(1500, rng)
    synth = train[rng.integers(0, len(train), 3000)]  # a copy, presented as "synthetic"
    for n, M in [("train", train), ("hold", hold), ("synth", synth)]:
        write(tmp_path / f"{n}.csv", M)
    dom = {c: [f"v{i}" for i in range(K)] for c in COLS[:-1]}
    dom["y"] = ["v0", "v1"]
    (tmp_path / "dom.json").write_text(json.dumps(dom))
    rep = evaluate(tmp_path / "train.csv", tmp_path / "hold.csv", tmp_path / "synth.csv",
                   tmp_path / "dom.json", "y", seed=1)
    assert not rep["passed"]
    assert not rep["checks"]["privacy: synthetic MIA AUC <= max"]
