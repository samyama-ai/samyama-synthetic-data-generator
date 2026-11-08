"""sdg command line.

    sdg generate configs/telecom/small.yaml --seed 7 --ontologies ../samyama-ontologies --out out/telecom-small
    sdg generate configs/telecom/small.yaml --topology ../graph_ws/telecom-kg/data/topology-zoo/Abilene.graphml \
                 --out out/telecom-abilene
    sdg validate out/telecom-small
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import yaml

DEFAULT_SEED = 20261019


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="sdg")
    sub = ap.add_subparsers(dest="cmd", required=True)
    g = sub.add_parser("generate", help="generate a dataset from a pack config")
    g.add_argument("config", type=Path)
    g.add_argument("--seed", type=int, default=DEFAULT_SEED)
    g.add_argument("--ontologies", type=Path,
                   default=Path(__file__).resolve().parents[3] / "samyama-ontologies",
                   help="samyama-ontologies checkout whose build/ holds the vocabulary builds")
    g.add_argument("--out", type=Path, required=True)
    g.add_argument("--set", dest="overrides", action="append", default=[], metavar="KEY=VALUE",
                   help="override one config value, dotted key, repeatable "
                        "(--set alarms.background_per_hour=0). For ablations: turning a "
                        "noise source off by editing a copy of the config lets everything "
                        "else drift with it, and then the difference is not the ablation. "
                        "The key must already exist and the value is parsed as YAML.")
    g.add_argument("--topology", type=Path, default=None,
                   help="override the config's topology. One config across several backbones is "
                        "how a topology comparison stays a comparison: copying the config per "
                        "backbone lets a rate drift with it and the difference stops being the "
                        "topology. Resolved the same way as the config's own value.")
    v = sub.add_parser("validate", help="conformance, ground-truth consistency, realism, baselines")
    v.add_argument("dir", type=Path)
    a = ap.parse_args(argv)
    if a.cmd == "generate":
        cfg = yaml.safe_load(a.config.read_text())
        if cfg.get("pack") != "telecom":
            ap.error(f"unknown pack {cfg.get('pack')!r}; available: telecom")
        for kv in a.overrides:
            if "=" not in kv:
                ap.error(f"--set wants KEY=VALUE, got {kv!r}")
            key, raw = kv.split("=", 1)
            parts, node = key.split("."), cfg
            for part in parts[:-1]:
                if not isinstance(node, dict) or part not in node:
                    ap.error(f"--set {key}: no such config key")
                node = node[part]
            if not isinstance(node, dict) or parts[-1] not in node:
                ap.error(f"--set {key}: no such config key")   # a typo must not be a silent no-op
            node[parts[-1]] = yaml.safe_load(raw)
        if a.topology is not None:
            cfg["topology"] = str(a.topology)
        topo = Path(cfg["topology"])
        if not topo.is_absolute():                     # relative to the repo's parent (the workspace)
            resolved = (Path(__file__).resolve().parents[3] / topo).resolve()
            if not resolved.exists():
                fixture_topo = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / topo.name
                if fixture_topo.exists():
                    resolved = fixture_topo
            cfg["topology"] = str(resolved)
        onto_build = a.ontologies / "build" if (a.ontologies / "build").exists() else a.ontologies
        if not onto_build.exists():
            onto_build = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "ontology-build"
        from .packs.telecom.pack import generate
        m = generate(cfg, a.seed, onto_build, a.out, a.config)
        print(json.dumps({k: m[k] for k in ("faults", "alarms", "incidents", "synthetic_share", "counts")},
                         indent=1), file=sys.stderr)
        return 0
    from .validate import validate
    report = validate(a.dir)
    print(json.dumps(report["summary"], indent=1), file=sys.stderr)
    return 0 if report["summary"]["conformance_errors"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
