"""A real Topology Zoo backbone, expanded into equipment, services and customers.

Only the backbone is real: PoPs and the links between them, from an Internet Topology Zoo
GraphML file (CC BY 4.0). Everything below a PoP is generated: core, aggregation, access
and RAN devices from fictional vendors, their cards and ports, the cabling between them,
and the services and customers that depend on them.

The shape follows how carrier networks are commonly built, as an assumption, not a
measurement: two core routers per PoP, every aggregation router dual-homed to both,
access and RAN sites single-homed to one aggregation router unless the dual-homing rate
says otherwise. Redundancy decides whether a failure costs service, so it is a knob.
"""

from __future__ import annotations

import xml.etree.ElementTree as ET
from collections import deque
from itertools import pairwise
from pathlib import Path
from typing import Any

from ...core.graph import Graph

NS = "{http://graphml.graphdrawing.org/xmlns}"

VENDORS = {                       # fictional, so no number here reads as a claim about a product
    "core": [("Altamira Networks", "AX-9600"), ("Corvex", "CR-8800")],
    "aggregation": [("Altamira Networks", "AX-4400"), ("Brightline Systems", "BL-AG40")],
    "access": [("Brightline Systems", "BL-210"), ("Duneport", "DP-ACC12")],
    "ran": [("Duneport", "DP-gNB3"), ("Kestrel Radio", "KR-5G2")],
}
LINE_CARDS = {"core": (4, 6), "aggregation": (2, 4), "access": (1, 2), "ran": (1, 1)}
PORTS_PER_CARD = {"core": 8, "aggregation": 8, "access": 4, "ran": 4}


def parse_graphml(path: Path) -> dict[str, Any]:
    """{name, pops: [{node_id, name, country, latitude, longitude}], links: [(a, b, label)]}"""
    root = ET.parse(path).getroot()
    keys = {k.get("id"): k.get("attr.name") for k in root.iter(f"{NS}key")}
    g = root.find(f"{NS}graph")
    pops, links = [], []
    for n in g.iter(f"{NS}node"):
        d = {keys.get(x.get("key")): (x.text or "") for x in n.iter(f"{NS}data")}
        pops.append({"node_id": int(n.get("id")), "name": d.get("label", n.get("id")),
                     "country": d.get("Country", ""),
                     "latitude": float(d["Latitude"]) if d.get("Latitude") else None,
                     "longitude": float(d["Longitude"]) if d.get("Longitude") else None})
    for e in g.iter(f"{NS}edge"):
        d = {keys.get(x.get("key")): (x.text or "") for x in e.iter(f"{NS}data")}
        a, b = int(e.get("source")), int(e.get("target"))
        if a != b:
            links.append((a, b, d.get("LinkLabel", "")))
    return {"name": path.stem, "pops": pops, "links": links}


class Network:
    """The generated inventory, with the indexes fault propagation needs."""

    def __init__(self) -> None:
        self.g = Graph()
        self.device_pop: dict[str, str] = {}
        self.device_role: dict[str, str] = {}
        self.device_cards: dict[str, list[str]] = {}
        self.card_ports: dict[str, list[str]] = {}
        self.port_card: dict[str, str] = {}
        self.card_device: dict[str, str] = {}
        self.peer: dict[str, str] = {}                      # port -> port at the other end
        self.uplinks: dict[str, list[tuple[str, str]]] = {}  # device -> [(local port, upstream device)]
        self.free: dict[str, deque[str]] = {}               # device -> unused ports
        self.pop_devices: dict[str, dict[str, list[str]]] = {}
        self.backbone: list[tuple[str, str]] = []           # (port, port) carrying a Topology Zoo link
        self.services: list[dict[str, Any]] = []
        self.pop_adj: dict[str, list[str]] = {}             # PoP -> neighbouring PoPs on the backbone

    def port_device(self, port: str) -> str:
        return self.card_device[self.port_card[port]]


def _device(net: Network, rng, pop_id: str, role: str, i: int) -> str:
    vendor, model = VENDORS[role][int(rng.integers(len(VENDORS[role])))]
    did = f"dev:{pop_id.split(':', 1)[1]}:{role[:3]}{i}"
    net.g.node(did, "Device", name=did.split(":", 1)[1], role=role, vendor=vendor, model=model,
               synthetic=True)
    net.g.edge(pop_id, "HOSTS", did)
    net.device_pop[did], net.device_role[did] = pop_id, role
    net.device_cards[did], net.free[did], net.uplinks[did] = [], deque(), []
    lo, hi = LINE_CARDS[role]
    for c in range(int(rng.integers(lo, hi + 1))):
        cid = f"{did}/c{c}"
        net.g.node(cid, "Card", device_id=did, slot=c, kind="line", synthetic=True)
        net.g.edge(did, "HAS_CARD", cid)
        net.device_cards[did].append(cid)
        net.card_device[cid], net.card_ports[cid] = did, []
        for p in range(PORTS_PER_CARD[role]):
            pid = f"{cid}/p{p}"
            net.g.node(pid, "Port", card_id=cid, index=p,
                       speed_gbps={"core": 100, "aggregation": 100, "access": 10, "ran": 25}[role],
                       synthetic=True)
            net.g.edge(cid, "HAS_PORT", pid)
            net.card_ports[cid].append(pid)
            net.port_card[pid] = cid
    # Spread cabling across cards: a card failure then takes some links, not all of them.
    order = [net.card_ports[c][p] for p in range(PORTS_PER_CARD[role]) for c in net.device_cards[did]]
    net.free[did].extend(order)
    return did


def connect(net: Network, a: str, b: str, at: int, link_id: str, backbone: bool = False) -> tuple[str, str]:
    if not net.free[a] or not net.free[b]:
        raise ValueError(f"out of ports connecting {a} and {b}; raise PORTS_PER_CARD")
    pa, pb = net.free[a].popleft(), net.free[b].popleft()
    net.g.edge(pa, "CONNECTS", pb, link_id=link_id, backbone=backbone, at=at)
    net.peer[pa], net.peer[pb] = pb, pa
    return pa, pb


def build(topo: dict[str, Any], cfg: dict[str, Any], rng, t0: int) -> Network:
    net = Network()
    n = topo["name"]
    for p in topo["pops"]:
        pid = f"pop:{n}:{p['node_id']}"
        net.g.node(pid, "PoP", network=n, synthetic=False, **p)
        roles: dict[str, list[str]] = {"core": [], "aggregation": [], "access": [], "ran": []}
        for i in range(cfg["core_per_pop"]):
            roles["core"].append(_device(net, rng, pid, "core", i))
        for i in range(int(rng.integers(cfg["aggregation_per_pop"][0], cfg["aggregation_per_pop"][1] + 1))):
            agg = _device(net, rng, pid, "aggregation", i)
            roles["aggregation"].append(agg)
            for core in roles["core"]:
                pa, _ = connect(net, agg, core, t0, f"{agg}~{core}")
                net.uplinks[agg].append((pa, core))
        for i in range(int(rng.integers(cfg["access_per_pop"][0], cfg["access_per_pop"][1] + 1))):
            role = "ran" if rng.random() < cfg["ran_share"] else "access"
            dev = _device(net, rng, pid, role, i)
            roles[role].append(dev)
            aggs = list(roles["aggregation"])
            first = aggs[int(rng.integers(len(aggs)))]
            homes = [first]
            if len(aggs) > 1 and rng.random() < cfg["dual_home_rate"]:
                homes.append(next(x for x in aggs if x != first))
            for agg in homes:
                pa, _ = connect(net, dev, agg, t0, f"{dev}~{agg}")
                net.uplinks[dev].append((pa, agg))
        net.pop_devices[pid] = roles
    for k, (a, b, label) in enumerate(topo["links"]):
        pa_id, pb_id = f"pop:{n}:{a}", f"pop:{n}:{b}"
        net.pop_adj.setdefault(pa_id, []).append(pb_id)
        net.pop_adj.setdefault(pb_id, []).append(pa_id)
        net.g.edge(pa_id, "LINK", pb_id, link_id=k, link_label=label)
        ca = net.pop_devices[pa_id]["core"][k % cfg["core_per_pop"]]
        cb = net.pop_devices[pb_id]["core"][k % cfg["core_per_pop"]]
        ports = connect(net, ca, cb, t0, f"bb:{n}:{k}", backbone=True)
        net.backbone.append(ports)
    for roles in net.pop_devices.values():   # the two cores of a PoP are cabled together
        cores = roles["core"]
        for x, y in pairwise(cores):
            connect(net, x, y, t0, f"{x}~{y}")
    _services(net, topo, cfg, rng, t0)
    return net


def _pop_graph(topo) -> dict[int, list[int]]:
    adj: dict[int, list[int]] = {p["node_id"]: [] for p in topo["pops"]}
    for a, b, _ in topo["links"]:
        adj[a].append(b)
        adj[b].append(a)
    return adj


def _path(adj, a: int, b: int, banned: set[int] = frozenset()) -> list[int] | None:
    prev, q = {a: None}, deque([a])
    while q:
        u = q.popleft()
        if u == b:
            out = [u]
            while prev[out[-1]] is not None:
                out.append(prev[out[-1]])
            return out[::-1]
        for v in sorted(adj[u]):
            if v not in prev and (v not in banned or v == b):
                prev[v] = u
                q.append(v)
    return None


def _services(net: Network, topo, cfg, rng, t0: int) -> None:
    g, n = net.g, topo["name"]
    k = 0

    def service(kind: str, sla: str, deps: list[tuple[str, str]], segment: str) -> None:
        nonlocal k
        sid = f"svc:{n}:{k}"
        g.node(sid, "Service", kind=kind, sla=sla, synthetic=True)
        for dst, role in deps:
            g.edge(sid, "DEPENDS_ON", dst, role=role, at=t0)
        cid = f"cust:{n}:{k}"
        g.node(cid, "Customer", segment=segment, synthetic=True)
        g.edge(cid, "SUBSCRIBES", sid)
        net.services.append({"id": sid, "kind": kind, "deps": deps})
        k += 1

    for roles in net.pop_devices.values():
        for dev in roles["ran"] + roles["access"]:
            kind = "mobile-data" if net.device_role[dev] == "ran" else "broadband"
            deps = [(dev, "primary")] + [(up, "primary" if i == 0 else "backup")
                                         for i, (_, up) in enumerate(net.uplinks[dev])]
            service(kind, "bronze" if kind == "broadband" else "silver", deps, "consumer")
    adj = _pop_graph(topo)
    pops = [p["node_id"] for p in topo["pops"]]
    for _ in range(cfg["enterprise_vpns"]):
        a, b = (int(x) for x in rng.choice(pops, size=2, replace=False))
        primary = _path(adj, a, b)
        if primary is None:
            continue
        backup = _path(adj, a, b, banned=set(primary[1:-1])) if len(primary) > 2 else None
        deps = []
        for role, path in (("primary", primary), ("backup", backup or [])):
            for pop in path:
                core = net.pop_devices[f"pop:{n}:{pop}"]["core"][0 if role == "primary" else -1]
                if (core, "primary") not in deps:
                    deps.append((core, role))
        for pop in (a, b):
            acc = net.pop_devices[f"pop:{n}:{pop}"]["access"] or net.pop_devices[f"pop:{n}:{pop}"]["aggregation"]
            deps.insert(0, (acc[int(rng.integers(len(acc)))], "primary"))
        service("enterprise-vpn", "gold", deps, "enterprise")
