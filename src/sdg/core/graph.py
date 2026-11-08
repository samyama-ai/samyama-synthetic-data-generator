"""The generated graph, its writers and its manifest.

Output shape is the one samyama-research-kit and samyama-ontologies already use, so the
same loaders read it:

    nodes.jsonl  {"id", "label", ...properties}
    edges.jsonl  {"src", "rel", "dst", ...properties}

Nodes are written sorted by id and edges in insertion order, which is itself a function of
the seed, so two runs with one seed produce identical bytes. The manifest records the seed,
the config, every input's sha256 and the output's sha256, so a dataset can be traced back
to exactly what made it.
"""

from __future__ import annotations

import hashlib
import json
from collections import Counter
from pathlib import Path
from typing import Any


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


class Graph:
    def __init__(self) -> None:
        self.nodes: dict[str, dict[str, Any]] = {}
        self.edges: list[dict[str, Any]] = []

    def node(self, nid: str, label: str, **props: Any) -> str:
        if nid in self.nodes:
            raise ValueError(f"duplicate node id {nid!r}")
        self.nodes[nid] = {"id": nid, "label": label, **props}
        return nid

    def edge(self, src: str, rel: str, dst: str, **props: Any) -> None:
        self.edges.append({"src": src, "rel": rel, "dst": dst, **props})

    def by_label(self, label: str) -> list[dict[str, Any]]:
        return [n for n in self.nodes.values() if n["label"] == label]

    def counts(self) -> dict[str, dict[str, int]]:
        return {"labels": dict(sorted(Counter(n["label"] for n in self.nodes.values()).items())),
                "relations": dict(sorted(Counter(e["rel"] for e in self.edges).items()))}

    def dangling(self) -> list[dict[str, Any]]:
        return [e for e in self.edges if e["src"] not in self.nodes or e["dst"] not in self.nodes]

    def write(self, out: Path) -> dict[str, str]:
        out.mkdir(parents=True, exist_ok=True)
        with (out / "nodes.jsonl").open("w") as f:
            for nid in sorted(self.nodes):
                f.write(json.dumps(self.nodes[nid], ensure_ascii=False, sort_keys=True) + "\n")
        with (out / "edges.jsonl").open("w") as f:
            for e in self.edges:
                f.write(json.dumps(e, ensure_ascii=False, sort_keys=True) + "\n")
        return {name: sha256_file(out / name) for name in ("nodes.jsonl", "edges.jsonl")}


def write_jsonl(path: Path, rows) -> str:
    with path.open("w") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n")
    return sha256_file(path)
