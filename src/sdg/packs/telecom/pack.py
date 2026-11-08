"""Telecom pack: one run = a network, a set of fault scenarios, and everything they leave behind.

    out/
      nodes.jsonl, edges.jsonl   the NOC layer on the real backbone (telecom-kg schema/telecom_noc.cypher)
      alarms.stream.jsonl        raise/clear events in time order, for replay into a live graph
      ground_truth.jsonl         per fault: type, root resources, onset, change, alarms caused
                                 and each caused alarm's mechanism (its role in the fault)
      binding.json               the ontology terms used, and the build files they came from
      manifest.json              seed, config, inputs and outputs with sha256, counts
      realism.json               written by `sdg validate`
"""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from ... import __version__
from ...core.graph import Graph, sha256_file, write_jsonl
from ...core.rng import Streams
from ...ontology import bind
from . import faults as F
from . import tickets as T
from .topology import build, parse_graphml

PACK_VERSION = "0.1.0"

# Labels -> ontology concepts (samyama-ontologies build ids), and where enum values come from.
SPEC = {
    "vocabularies": ["noria-o", "ietf-alarms", "ietf-network-topology", "tmf642"],
    "labels": {
        "PoP": [],
        "Device": ["noria:Resource", "ietf-network:networks/network/node"],
        "Card": ["noria:Resource"],
        "Port": ["noria:NetworkInterface", "ietf-network-topology:networks/network/node/termination-point"],
        "Service": ["noria:Service"],
        "Customer": [],
        "Alarm": ["noria:EventRecord", "ietf-alarms:alarms/alarm-list/alarm", "tmf642:Alarm"],
        "Incident": ["noria:TroubleTicket"],
        "Change": ["noria:ChangeRequest"],
        "Runbook": [],
        "Fault": [],
    },
    "enums": {
        "perceived_severity": {"vocabulary": "ietf-alarms", "owner": "ietf-alarms:typedef/severity"},
        "alarm_type": {"vocabulary": "tmf642", "owner": "tmf642:AlarmType"},
        "probable_cause": {"vocabulary": "noria-o", "scheme": "ProbableCause"},
    },
}


def _ms(iso: str) -> int:
    return int(datetime.fromisoformat(iso).replace(tzinfo=timezone.utc).timestamp() * 1000)


def generate(cfg: dict[str, Any], seed: int, ontologies_build: Path, out: Path,
             config_path: Path | None = None) -> dict[str, Any]:
    rs = Streams(seed)
    binding = bind(ontologies_build, SPEC)
    topo_path = Path(cfg["topology"])
    topo = parse_graphml(topo_path)
    t0 = _ms(cfg["start"])
    net = build(topo, cfg["network"], rs("topology"), t0)
    g: Graph = net.g
    skew_rng = rs("skew")
    skew = {d: int(skew_rng.normal(0, cfg["alarms"]["clock_skew_ms"])) for d in sorted(net.device_role)}
    degree: dict[str, int] = {}
    for a, b, _ in topo["links"]:
        for x in (a, b):
            degree[f"pop:{topo['name']}:{x}"] = degree.get(f"pop:{topo['name']}:{x}", 0) + 1
    noc_pop = max(sorted(degree), key=lambda p: degree[p])

    af = F.AlarmFactory(net, binding, cfg, rs("alarms"), skew)
    frng, trng, crng = rs("faults"), rs("tickets"), rs("changes")
    nrng = rs("notify")                      # per-element send spacing; its own stream
    slot = cfg["slot_hours"] * F.HOUR
    truth, all_incidents, changes = [], [], []
    i = 0
    for sc in cfg["scenarios"]:
        for _ in range(sc["count"]):
            scenario = f"{sc['split']}-{sc['kind']}-{i:04d}"
            start = t0 + i * slot
            onset = start + int(frng.integers(slot // 5, slot // 2))
            types, weights = zip(*sorted(sc["fault_types"].items()))
            p = [w / sum(weights) for w in weights]
            ft = str(frng.choice(types, p=p))
            fs = [F.pick_fault(net, frng, ft, onset, cfg["repair_median_ms"], f"flt:{scenario}:0", scenario)]
            if sc["kind"] == "overlap":
                pop = net.device_pop.get(af.device_of(fs[0].resources[0])) if not fs[0].resources[0].startswith(
                    "pop:") else fs[0].resources[0]
                ft2 = str(frng.choice(types, p=p))
                fs.append(F.pick_fault(net, frng, ft2, onset + int(frng.integers(0, cfg["overlap_window_ms"])),
                                       cfg["repair_median_ms"], f"flt:{scenario}:1", scenario, pop=pop))
            for f in fs:
                if f.fault_type == "config_change" or frng.random() < cfg["changes"]["fault_after_change_prob"]:
                    target = f.resources[0] if not f.resources[0].startswith("pop:") else None
                    if target:
                        cid = f"chg:{scenario}:{f.id.rsplit(':', 1)[1]}"
                        lead = int(crng.integers(1 * F.MIN, 20 * F.MIN))
                        changes.append({"id": cid, "kind": "software-upgrade" if f.fault_type != "config_change"
                                        else "configuration", "planned_start": f.onset - lead - 10 * F.MIN,
                                        "actual_start": f.onset - lead, "outcome": "failed", "target": target})
                        f.change = cid
            window = (start, start + slot)
            n0 = len(af.alarms)
            F.simulate(net, fs, scenario, window, noc_pop, af)
            mine = af.alarms[n0:]
            # Before tickets are cut, because a ticket's opened_at is derived from its
            # first alarm's time. One element's alarms leave it one at a time.
            af.serialise(mine, nrng, cfg["alarms"]["element_notify_ms"])
            bg = [a for a in mine if a["mechanism"] == "background"]
            for f in fs:
                caused = {aid for fid, aid, _, _ in af.caused if fid == f.id}
                mech = {aid: m for fid, aid, m, _ in af.caused if fid == f.id}
                all_incidents += T.incidents(f, mine, caused, bg, sc["split"], net, cfg, trng, f.change)
                truth.append({"fault": f.id, "scenario": scenario, "split": sc["split"], "kind": sc["kind"],
                              "fault_type": f.fault_type, "resources": f.resources, "onset": f.onset,
                              "repair": f.repair, "change": f.change, "alarms": sorted(caused),
                              # Each caused alarm's role in the fault. Ground truth: it is in
                              # this file and on the CAUSED edge, and on no alarm.
                              "alarm_mechanisms": {a: mech[a] for a in sorted(caused)},
                              "window": list(window)})
            for _ in range(int(crng.poisson(cfg["changes"]["unrelated_per_slot"]))):   # changes that broke nothing
                devs = sorted(net.device_role)
                at = int(crng.integers(*window))
                changes.append({"id": f"chg:{scenario}:u{len(changes)}", "kind": "configuration",
                                "planned_start": at - 10 * F.MIN, "actual_start": at, "outcome": "success",
                                "target": devs[int(crng.integers(len(devs)))]})
            i += 1

    # ---- emit -------------------------------------------------------------------------
    for a in af.alarms:
        res = a["resource"]
        # "mechanism" is the alarm's role in the fault that caused it: ground truth, and
        # dropped here. It travels on the CAUSED edge and in ground_truth.jsonl instead.
        g.node(a["id"], "Alarm", **{k: v for k, v in a.items() if k not in ("id", "resource", "mechanism")},
               state="raised" if a["cleared_at"] is None else "cleared", synthetic=True)
        g.edge(a["id"], "RAISED_ON", res)
    for c in changes:
        g.node(c["id"], "Change", **{k: v for k, v in c.items() if k not in ("id", "target")}, synthetic=True)
        g.edge(c["id"], "TARGETS", c["target"])
    kinds = sorted({T.kind_of(net, a["resource"]) for a in af.alarms})
    rbs = T.runbooks(sorted({a["alarm_type"] for a in af.alarms}), kinds)
    for rb in rbs:
        g.node(rb["id"], "Runbook", **{k: v for k, v in rb.items() if k != "id"}, synthetic=True)
    rb_ids = {rb["id"] for rb in rbs}
    for inc in all_incidents:
        g.node(inc["id"], "Incident", **{k: v for k, v in inc.items() if k not in ("id", "alarms", "runbook")})
        for aid in inc["alarms"]:
            g.edge(aid, "PART_OF", inc["id"])
        if inc["runbook"] in rb_ids and inc["split"] == "history":
            g.edge(inc["id"], "RESOLVED_WITH", inc["runbook"])
    for t in truth:
        g.node(t["fault"], "Fault", fault_type=t["fault_type"], onset=t["onset"], scenario=t["scenario"],
               synthetic=True)
        for r in t["resources"]:
            g.edge(t["fault"], "AT", r)
        if t["change"]:
            g.edge(t["fault"], "TRIGGERED_BY", t["change"])
    for fid, aid, mech, hops in af.caused:
        g.edge(fid, "CAUSED", aid, via=mech, hops=hops)
    for label, concepts in binding.labels.items():
        sl = g.node(f"schema:{label}", "SchemaLabel", name=label, synthetic=False)
        for c in concepts:
            oc = f"onto:{c}"
            if oc not in g.nodes:
                g.node(oc, "OntologyConcept", concept=c, vocabulary=c.split(":", 1)[0], synthetic=False)
            g.edge(sl, "GROUNDED_IN", oc)

    dangling = g.dangling()
    if dangling:
        raise RuntimeError(f"{len(dangling)} edges point at nodes that do not exist, e.g. {dangling[:2]}")
    out.mkdir(parents=True, exist_ok=True)
    shas = g.write(out)
    events = sorted([{"t": a["raised_at"], "event": "raise", "alarm": a["id"]} for a in af.alarms]
                    + [{"t": a["cleared_at"], "event": "clear", "alarm": a["id"]} for a in af.alarms
                       if a["cleared_at"] is not None], key=lambda e: (e["t"], e["alarm"], e["event"]))
    shas["alarms.stream.jsonl"] = write_jsonl(out / "alarms.stream.jsonl", events)
    shas["ground_truth.jsonl"] = write_jsonl(out / "ground_truth.jsonl", truth)
    (out / "binding.json").write_text(json.dumps(binding.to_json(), indent=1, sort_keys=True))
    counts = g.counts()
    synth = sum(1 for n in g.nodes.values() if n.get("synthetic"))
    manifest = {
        "generator": f"samyama-sdg {__version__}", "pack": f"telecom {PACK_VERSION}", "seed": seed,
        # File names, not paths: a path names the machine that built it, not the data.
        "config": {**cfg, "topology": topo_path.name}, "config_file": config_path.name if config_path else None,
        "inputs": {"topology": {"file": topo_path.name, "sha256": sha256_file(topo_path),
                                "license": "CC BY 4.0 (Internet Topology Zoo)"},
                   "ontology_builds": binding.inputs},
        "outputs": shas, "counts": counts,
        "synthetic_share": round(synth / len(g.nodes), 4),
        "faults": len(truth), "alarms": len(af.alarms), "incidents": len(all_incidents),
        "noc_pop": noc_pop,
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True, default=str))
    return manifest
