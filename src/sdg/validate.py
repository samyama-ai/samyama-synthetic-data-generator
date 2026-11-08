"""Check a generated dataset, describe it, and score simple RCA baselines against it.

Four parts, written to realism.json beside the data:

1. Conformance: every enumerated value is one the binding resolved from the ontologies;
   every edge's endpoints exist.
2. Ground truth: every caused alarm is raised no earlier than its fault's onset (allowing
   for clock skew), and background alarms carry no CAUSED edge.
3. Realism: alarms per fault, storm peak per minute, severity and type mix, the share of
   faults whose root raised no alarm, and so on. These describe the data. They are not
   claims about any real network: the rates behind them are assumed.
4. Baselines, on the eval split, so a graph method has a bar to beat. Each ranks
   candidate devices for one fault's incident; a hit is the device of any resource the fault
   was at (a fibre cut has two ends), or the PoP for a site power loss:
   - most_recent_change: devices changed in the 2 h before the first alarm, newest first
     (the itbench-kg baseline that ranks first in 12 of 18 ITBench scenarios);
   - earliest_critical: the devices of the earliest critical alarms;
   - most_alarmed: devices by alarm count;
   - random_alarmed: a random alarmed device (the floor).
"""

from __future__ import annotations

import json
from collections import Counter, defaultdict
from pathlib import Path
from statistics import median
from typing import Any

import numpy as np


def _load(d: Path):
    nodes = {}
    for line in (d / "nodes.jsonl").open():
        n = json.loads(line)
        nodes[n["id"]] = n
    edges = [json.loads(line) for line in (d / "edges.jsonl").open()]
    truth = [json.loads(line) for line in (d / "ground_truth.jsonl").open()]
    binding = json.loads((d / "binding.json").read_text())
    manifest = json.loads((d / "manifest.json").read_text())
    return nodes, edges, truth, binding, manifest


def _pct(xs, q):
    return float(np.percentile(xs, q)) if xs else None


def validate(d: Path) -> dict[str, Any]:
    nodes, edges, truth, binding, manifest = _load(d)
    errors: list[str] = []
    allowed = {p: {v["value"] for v in vs} for p, vs in binding["enums"].items()}
    alarms = {i: n for i, n in nodes.items() if n["label"] == "Alarm"}
    for a in alarms.values():
        for p, ok in allowed.items():
            if a.get(p) not in ok:
                errors.append(f"{a['id']}: {p}={a.get(p)!r} not in the bound vocabulary")
    for e in edges:
        if e["src"] not in nodes or e["dst"] not in nodes:
            errors.append(f"dangling edge {e}")
    unbound = sorted({n["label"] for n in nodes.values()} - set(binding["labels"])
                     - {"SchemaLabel", "OntologyConcept"})
    if unbound:
        errors.append(f"labels with no binding entry: {unbound}")

    # ---- ground truth ----
    skew = 4 * manifest["config"]["alarms"]["clock_skew_ms"]
    onset = {t["fault"]: t["onset"] for t in truth}
    caused_by: dict[str, str] = {}
    # The alarm's role in the fault -- root, loss-of-signal, isolation, neighbour, service,
    # congestion -- is ground truth and reaches the dataset only on the CAUSED edge, so it is
    # read from there and not from the alarm. An alarm with no CAUSED edge is background.
    mechanism: dict[str, str] = {}
    for e in edges:
        if e["rel"] == "CAUSED":
            caused_by[e["dst"]] = e["src"]
            mechanism[e["dst"]] = e.get("via")
            if alarms[e["dst"]]["raised_at"] < onset[e["src"]] - skew:
                errors.append(f"{e['dst']} raised before its fault {e['src']} began")
    # An alarm cannot clear before it is raised. This went unchecked and 11 alarms in the
    # dataset published on 2026-10-21 had cleared_at < raised_at: a flapping alarm walks its
    # raise forward through up to four cycles while its clear stays at the fault's repair.
    inverted = sorted(a["id"] for a in alarms.values()
                      if a.get("cleared_at") is not None and a["cleared_at"] < a["raised_at"])
    if inverted:
        errors.append(f"{len(inverted)} alarms clear before they are raised, e.g. {inverted[:3]}")
    bg_caused = [a for a, m in mechanism.items() if m == "background"]
    if bg_caused:
        errors.append(f"{len(bg_caused)} background alarms have a CAUSED edge")
    # No alarm property may spell out a mechanism. Only the names that are not also
    # vocabulary values count: "congestion" is a NORIA-O probable cause as well as a
    # mechanism, and an alarm whose cause is congestion is realistic, not a leak.
    tell = (set(mechanism.values()) | {"background"}) - {v for vs in allowed.values() for v in vs}
    leaked = sorted({(p, v) for a in alarms.values() for p, v in a.items()
                     if isinstance(v, str) and v in tell})
    if leaked:
        errors.append(f"alarm properties spell out a ground-truth mechanism: {leaked}")
    truth_mech = {a: m for t in truth for a, m in t.get("alarm_mechanisms", {}).items()}
    if truth_mech != mechanism:
        errors.append("ground_truth.jsonl and the CAUSED edges disagree about alarm mechanisms")

    # ---- realism ----
    per_fault = [len(t["alarms"]) for t in truth]
    root_alarmed = sum(1 for t in truth if any(mechanism.get(a) == "root" for a in t["alarms"]))
    by_minute = Counter(a["raised_at"] // 60_000 for a in alarms.values())
    realism = {
        "faults": len(truth), "alarms": len(alarms),
        "alarms_per_fault": {"median": median(per_fault) if per_fault else None,
                             "p90": _pct(per_fault, 90), "max": max(per_fault, default=None)},
        "faults_with_no_alarm": sum(1 for x in per_fault if x == 0),
        "faults_whose_root_raised_an_alarm": root_alarmed,
        "peak_alarms_per_minute": max(by_minute.values(), default=0),
        "background_share": round(sum(1 for i in alarms if i not in mechanism) / max(len(alarms), 1), 3),
        "severity_mix": dict(Counter(a["perceived_severity"] for a in alarms.values()).most_common()),
        "alarm_type_mix": dict(Counter(a["alarm_type"] for a in alarms.values()).most_common()),
        "mechanism_mix": dict(Counter(mechanism.get(i, "background") for i in alarms).most_common()),
        "specific_problem_mix": dict(Counter(a["specific_problem"] for a in alarms.values()).most_common()),
        "faults_after_a_change": sum(1 for t in truth if t["change"]),
        "fault_types": dict(Counter(t["fault_type"] for t in truth).most_common()),
    }

    # ---- baselines ----
    raised_on = {e["src"]: e["dst"] for e in edges if e["rel"] == "RAISED_ON"}
    card_dev = {e["dst"]: e["src"] for e in edges if e["rel"] == "HAS_CARD"}
    port_card = {e["dst"]: e["src"] for e in edges if e["rel"] == "HAS_PORT"}
    dev_pop = {e["dst"]: e["src"] for e in edges if e["rel"] == "HOSTS"}

    def device(r: str) -> str:
        if r in port_card:
            r = port_card[r]
        return card_dev.get(r, r)

    targets = {e["src"]: e["dst"] for e in edges if e["rel"] == "TARGETS"}
    changes = [(n["actual_start"], device(targets[i])) for i, n in nodes.items() if n["label"] == "Change"]
    members = defaultdict(list)
    for e in edges:
        if e["rel"] == "PART_OF":
            members[e["dst"]].append(e["src"])
    rng = np.random.default_rng(0)
    scores: dict[str, list[int]] = defaultdict(list)       # method -> rank of the truth (0 = miss)
    # The same ranks split by fault type. An aggregate hides which class a rule cannot do:
    # on site power loss the dead PoP reports nothing, and a rule that ranks alarmed
    # devices by severity has no way in, while a method that reads the topology does.
    by_type: dict[str, dict[str, list[int]]] = defaultdict(lambda: defaultdict(list))
    evaluated = 0
    for t in truth:
        if t["split"] != "eval":
            continue
        inc = f"inc:{t['fault']}"
        al = [alarms[a] for a in members.get(inc, []) if a in alarms]
        evaluated += 1
        if not al:
            # An eval incident that raises no alarm anywhere. It stays in the denominator
            # and every method is charged a miss for it.
            #
            # Skipping it was the earlier behaviour and it understated the task: an
            # operator whose fault produces no alarm is not excused from having missed
            # it, and a benchmark that drops what no method can see reports a
            # denominator that does not exist. On the GEANT small configuration two
            # incidents are in this class -- both the second fault of an overlapping
            # pair on a resource already alarming -- and dropping them moved every rate
            # by 38/40.
            for m in ("most_recent_change", "earliest_critical", "most_alarmed",
                      "random_alarmed"):
                scores[m].append(0)                       # 0 is this function's miss
                by_type[t["fault_type"]][m].append(0)
            continue
        hit = {r if r.startswith("pop:") else device(r) for r in t["resources"]}   # a fibre cut has two ends

        def rank(cands: list[str], hit: set[str] = hit) -> int:
            seen = []
            for c in cands:
                if c not in seen:
                    seen.append(c)
            for k, c in enumerate(seen[:3], 1):
                if c in hit or dev_pop.get(c) in hit:
                    return k
            return 0

        first = min(a["raised_at"] for a in al)
        recent = sorted((c for c in changes if first - 7_200_000 <= c[0] <= first), reverse=True)
        crit = sorted((a for a in al if a["perceived_severity"] == "critical"), key=lambda a: a["raised_at"])
        cnt = Counter(device(raised_on[a["id"]]) for a in al)
        devs = sorted(cnt)
        got = {
            "most_recent_change": rank([dev for _, dev in recent]),
            "earliest_critical": rank([device(raised_on[a["id"]]) for a in crit]),
            "most_alarmed": rank([d for d, _ in cnt.most_common()]),
            "random_alarmed": rank([devs[int(i)] for i in rng.permutation(len(devs))]),
        }
        for m, r in got.items():
            scores[m].append(r)
            by_type[t["fault_type"]][m].append(r)

    def agg(rs: list[int]) -> dict[str, Any]:
        return {"top1": round(sum(1 for r in rs if r == 1) / len(rs), 3),
                "top3": round(sum(1 for r in rs if 1 <= r <= 3) / len(rs), 3), "n": len(rs)}
    baselines = {m: {k: v for k, v in agg(rs).items() if k != "n"}
                 for m, rs in scores.items()} if evaluated else {}
    baselines_by_fault_type = ({k: {m: agg(v) for m, v in sorted(d.items())}
                                for k, d in sorted(by_type.items())} if evaluated else {})

    report = {"summary": {"conformance_errors": len(errors), "eval_incidents_scored": evaluated,
                          "baselines": baselines,
                          "baselines_by_fault_type": baselines_by_fault_type, **{k: realism[k] for k in ("faults", "alarms",
                                                                           "peak_alarms_per_minute")}},
              "errors": errors[:50], "realism": realism, "baselines": baselines,
              "baselines_by_fault_type": baselines_by_fault_type}
    (d / "realism.json").write_text(json.dumps(report, indent=1, sort_keys=True))
    return report
