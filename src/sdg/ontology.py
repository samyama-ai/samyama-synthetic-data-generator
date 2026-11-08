"""Bind a pack's terms to the vocabularies in samyama-ontologies.

A pack names, for each label it emits, the ontology concepts that label stands for, and
for each enumerated property, where its values come from. This module reads the
vocabulary builds (`samyama-ontologies/build/<vocab>/nodes.jsonl`, made by its converters)
and checks every name. **A term the ontology does not have stops the run**: the point of
binding is that generated data speaks the standard's words, and a silently invented one
defeats it. A pack that needs a term no vocabulary has lists it as a local extension.

Enumerations are resolved two ways, matching how the converters represent them:
- `owner`: values are the nodes with a VALUE_OF edge to that node (YANG enums, OpenAPI enums);
- `scheme`: values are the SKOS concepts in that scheme (NORIA-O code lists).
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .core.graph import sha256_file


class BindingError(ValueError):
    pass


@dataclass
class Binding:
    labels: dict[str, list[str]] = field(default_factory=dict)        # label -> concept ids
    enums: dict[str, list[dict[str, str]]] = field(default_factory=dict)  # prop -> [{value, concept}]
    local: list[str] = field(default_factory=list)                    # labels with no concept
    inputs: dict[str, str] = field(default_factory=dict)              # build file -> sha256

    def values(self, prop: str) -> list[str]:
        return [v["value"] for v in self.enums[prop]]

    def concept(self, prop: str, value: str) -> str:
        return next(v["concept"] for v in self.enums[prop] if v["value"] == value)

    def to_json(self) -> dict[str, Any]:
        return {"labels": self.labels, "enums": self.enums, "local_extensions": self.local,
                "inputs": self.inputs}


def _read(build: Path, vocab: str, inputs: dict[str, str]) -> tuple[dict[str, dict], list[dict]]:
    d = build / vocab
    if not (d / "nodes.jsonl").exists():
        raise BindingError(f"no build for {vocab!r} under {build}; run samyama-ontologies' converter "
                           f"for it first (README: 'Telecom (network operations)')")
    nodes = {}
    for line in (d / "nodes.jsonl").open():
        n = json.loads(line)
        nodes[n["id"]] = n
    edges = [json.loads(line) for line in (d / "edges.jsonl").open()]
    for name in ("nodes.jsonl", "edges.jsonl"):
        inputs[f"{vocab}/{name}"] = sha256_file(d / name)
    return nodes, edges


def bind(build: Path, spec: dict[str, Any]) -> Binding:
    """spec = {"vocabularies": [...], "labels": {label: [concept, ...] | []},
               "enums": {prop: {"vocabulary", "owner" | "scheme", "exclude"?}}}"""
    b = Binding()
    data = {v: _read(build, v, b.inputs) for v in spec["vocabularies"]}
    known = {cid for nodes, _ in data.values() for cid in nodes}
    for label, concepts in spec["labels"].items():
        missing = [c for c in concepts if c not in known]
        if missing:
            raise BindingError(f"label {label!r}: concepts not in the vocabularies: {missing}")
        if not concepts:
            b.local.append(label)
        b.labels[label] = list(concepts)
    for prop, how in spec["enums"].items():
        nodes, edges = data[how["vocabulary"]]
        if "owner" in how:
            if how["owner"] not in nodes:
                raise BindingError(f"enum {prop!r}: owner {how['owner']!r} not in {how['vocabulary']}")
            ids = [e["src"] for e in edges if e["rel"] == "VALUE_OF" and e["dst"] == how["owner"]]
        else:
            ids = sorted(i for i, n in nodes.items()
                         if how["scheme"] == n.get("scheme")
                         or how["scheme"] in (n.get("scheme") if isinstance(n.get("scheme"), list) else []))
        vals, seen = [], set()
        for i in ids:                       # NORIA-O lists adapterError under 5 event types: one value
            name = nodes[i]["name"]
            if name in seen or name in set(how.get("exclude", [])):
                continue
            seen.add(name)
            vals.append({"value": name, "concept": i})
        if not vals:
            raise BindingError(f"enum {prop!r}: no values found in {how['vocabulary']} for {how}")
        b.enums[prop] = vals
    return b
