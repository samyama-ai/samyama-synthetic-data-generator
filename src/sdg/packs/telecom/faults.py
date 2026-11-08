"""Faults, how they spread, and the alarms an operator would see, with ground truth.

A fault takes resources down. The network's state then follows from the topology, not from
a script: a dead card takes its ports down; the far end of each of those ports loses
signal; a device whose every path to the NOC is gone becomes unreachable; a service whose
endpoint is unreachable is out. Each of those states raises an alarm with some probability
and some delay, on the resource that would report it, in the vocabulary's terms
(RFC 8632 severities, X.733/TMF642 alarm types, NORIA-O probable causes).

Storms come from the mechanisms that make them in mobile networks (assumed, as below): when
a radio site goes down or dark, each live neighbouring radio site raises a link-failure
alarm per cell (the X2/Xn neighbour relation), and the core raises a service alarm for
each mobile service it loses.

What makes the data hard, on purpose, and each a config knob:
- the root resource often raises no alarm at all (a dead device cannot report itself);
- alarms go missing, flap, duplicate, and carry per-device clock skew;
- unrelated background alarms arrive throughout;
- two faults can overlap in one region.

Every alarm a fault causes carries a CAUSED edge from the Fault, with the mechanism and hop
count. Background alarms carry none. That is the ground truth RCA methods are scored on.

Rates, delays and probabilities are ASSUMED: no openly licensed real telecom alarm log was
found to calibrate them (samyama-ontologies#9 research notes). They are in config so a
partner's data can replace them.
"""

from __future__ import annotations

import math
import re
from collections import deque
from dataclasses import dataclass, field
from typing import Any

from .topology import Network

MIN = 60_000
HOUR = 60 * MIN

CAUSES = {          # mechanism -> NORIA-O probable causes (checked against the binding at run time)
    "card_failure": ["equipmentMalfunction", "receiverFailure", "transmitterFailure"],
    "device_failure": ["equipmentMalfunction", "processorProblem"],
    "software_crash": ["softwareProgramAbnormallyTerminated", "softwareError", "outOfMemory"],
    "power_loss": ["powerProblem"],
    "port_failure": ["lossOfSignal", "transmitterFailure"],
    "fibre_cut": ["lossOfSignal"],
    "config_change": ["configurationOrCustomizationError"],
    "loss-of-signal": ["lossOfSignal", "lossOfFrame", "degradedSignal", "remoteNodeTransmissionError"],
    "isolation": ["communicationsSubsystemFailure"],
    "service": ["performanceDegraded", "bandwidthReduced", "responseTimeExcessive"],
    "congestion": ["congestion", "thresholdCrossed", "resourceAtOrNearingCapacity"],
    "neighbour": ["communicationsProtocolError", "remoteNodeTransmissionError"],
    "background": ["thresholdCrossed", "temperatureUnacceptable", "performanceDegraded",
                   "heatingOrVentilationOrCoolingSystemProblem", "bandwidthReduced"],
}
ALARM_TYPE = {
    "card_failure": "equipmentAlarm", "device_failure": "equipmentAlarm",
    "software_crash": "processingErrorAlarm", "power_loss": "environmentalAlarm",
    "port_failure": "communicationsAlarm", "fibre_cut": "communicationsAlarm",
    "config_change": "processingErrorAlarm", "loss-of-signal": "communicationsAlarm",
    "isolation": "communicationsAlarm", "service": "qualityOfServiceAlarm",
    "congestion": "qualityOfServiceAlarm", "neighbour": "communicationsAlarm",
}
SEVERITY = {"root": "critical", "loss-of-signal": "major", "isolation": "critical",
            "service": "major", "congestion": "warning", "neighbour": "minor"}

# X.733 specificProblem: the equipment's own name for what it reported, refining
# probable_cause. It is a function of probable_cause and nothing else, so it carries no
# information the cause does not already carry.
#
# It used to carry the MECHANISM instead -- the alarm's role in the fault (root, neighbour,
# background, ...). That shipped the answer as an ordinary alarm property: on the GEANT
# small dataset, `specific_problem == "root"` named a ground-truth root resource in all 28
# of the 40 eval incidents where such an alarm existed, so one line of code scored 0.700
# top-1 with no reasoning. The role is ground truth and now lives only there: on the
# Fault-[:CAUSED {via}]->Alarm edge, and per alarm in ground_truth.jsonl. A background
# alarm is one with no CAUSED edge.
SPECIFIC_PROBLEM = {
    "equipmentMalfunction": "Board hardware fault",
    "receiverFailure": "Receive module failure",
    "transmitterFailure": "Transmit module failure",
    "processorProblem": "Main processing unit fault",
    "softwareProgramAbnormallyTerminated": "Process terminated unexpectedly",
    "softwareError": "Software exception raised",
    "outOfMemory": "Memory pool exhausted",
    "powerProblem": "Power input failure",
    "lossOfSignal": "R_LOS",
    "lossOfFrame": "R_LOF",
    "degradedSignal": "Bit error rate over threshold",
    "remoteNodeTransmissionError": "Remote defect indication",
    "configurationOrCustomizationError": "Configuration data inconsistent",
    "communicationsSubsystemFailure": "NE not reachable",
    "communicationsProtocolError": "Neighbour relation down",
    "performanceDegraded": "Service KPI below objective",
    "bandwidthReduced": "Throughput below committed rate",
    "responseTimeExcessive": "Round-trip delay over threshold",
    "congestion": "Interface congestion",
    "thresholdCrossed": "Performance counter over threshold",
    "resourceAtOrNearingCapacity": "Resource utilisation high",
    "temperatureUnacceptable": "Temperature over threshold",
    "heatingOrVentilationOrCoolingSystemProblem": "Fan unit failure",
}


def specific_problem_for(cause: str) -> str:
    """The alarm text for a probable cause. A pure function of the cause, by design.

    A cause the table does not name falls back to its own words, so a vocabulary that
    gains a term still produces a readable alarm rather than a leak or a blank.
    """
    if cause in SPECIFIC_PROBLEM:
        return SPECIFIC_PROBLEM[cause]
    words = re.sub(r"(?<!^)(?=[A-Z])", " ", cause).lower()
    return words[:1].upper() + words[1:]


@dataclass
class Fault:
    id: str
    fault_type: str
    resources: list[str]          # where it is: a card, a device, a port pair, or a PoP
    onset: int
    repair: int
    scenario: str
    change: str | None = None
    ports: list[str] = field(default_factory=list)   # config_change: the interfaces it broke


@dataclass
class Outcome:
    down_ports: set[str] = field(default_factory=set)
    down_devices: set[str] = field(default_factory=set)
    los_ports: set[str] = field(default_factory=set)
    isolated: set[str] = field(default_factory=set)
    services_out: list[dict[str, Any]] = field(default_factory=list)


def effect(net: Network, faults: list[Fault], noc_pop: str) -> Outcome:
    """What is down, dark or unreachable while these faults are active."""
    o = Outcome()
    for f in faults:
        for r in f.resources:
            if r.startswith("pop:"):
                for devs in net.pop_devices[r].values():
                    o.down_devices.update(devs)
            elif r in net.device_cards:
                if f.fault_type == "config_change":       # chosen when the fault was picked
                    o.down_ports.update(f.ports)
                else:
                    o.down_devices.add(r)
            elif r in net.card_ports:
                o.down_ports.update(net.card_ports[r])
            else:
                o.down_ports.add(r)
    for d in o.down_devices:
        for c in net.device_cards[d]:
            o.down_ports.update(net.card_ports[c])
    o.los_ports = {net.peer[p] for p in o.down_ports if p in net.peer} - o.down_ports
    # Reachability from the NOC over working links: an alive device that cannot be reached is
    # isolated. Redundancy (dual homing, two cores) is what keeps a failure from spreading.
    start = [d for d in net.pop_devices[noc_pop]["core"] if d not in o.down_devices]
    seen, q = set(start), deque(start)
    links: dict[str, list[tuple[str, str]]] = {}
    for p, peer in net.peer.items():
        links.setdefault(net.port_device(p), []).append((p, peer))
    while q:
        u = q.popleft()
        for p, peer in links.get(u, []):
            v = net.port_device(peer)
            if v in seen or v in o.down_devices or p in o.down_ports or peer in o.down_ports:
                continue
            seen.add(v)
            q.append(v)
    o.isolated = {d for d in net.device_role if d not in seen and d not in o.down_devices}
    dead = o.down_devices | o.isolated
    for s in net.services:
        ends = [d for d, role in s["deps"][: 2 if s["kind"] == "enterprise-vpn" else 1]]
        if any(d in dead for d in ends):
            o.services_out.append({**s, "dead_ends": [d for d in ends if d in dead],
                                   "live_ends": [d for d in ends if d not in dead]})
    return o


class AlarmFactory:
    def __init__(self, net: Network, binding, cfg: dict[str, Any], rng, skew: dict[str, int]):
        self.net, self.b, self.cfg, self.rng, self.skew = net, binding, cfg, rng, skew
        self.causes = {k: [c for c in v if c in binding.values("probable_cause")] or
                       binding.values("probable_cause") for k, v in CAUSES.items()}
        self.alarms: list[dict[str, Any]] = []
        self.caused: list[tuple[str, str, str, int]] = []      # (fault, alarm, mechanism, hops)
        self.k = 0

    def device_of(self, r: str) -> str:
        if r in self.net.device_cards:
            return r
        if r in self.net.card_device:
            return self.net.card_device[r]
        return self.net.port_device(r)

    def serialise(self, alarms: list[dict], rng, median_ms: float, sigma: float = 0.6) -> int:
        """Space one element's alarms: an element reports through one agent.

        Two notifications from the same device cannot leave it at the same instant: the
        agent builds and sends them one after another. This enforces that, and its effect on
        the stream is small -- the sub-second inter-arrival share moves 0.046 -> 0.040,
        because most sub-second pairs are two different elements reporting at once, which is
        not an artefact. The neighbour reporting period above is what moved that statistic
        (0.205 -> 0.046).

        Only `raised_at` moves, forward, and each alarm keeps its duration so a clear never
        precedes its raise. The draw comes from its own stream, so no other alarm property
        changes. `median_ms` is assumed, like every other rate in this file.
        """
        by_dev: dict[str, list[dict]] = {}
        for a in alarms:
            by_dev.setdefault(self.device_of(a["resource"]), []).append(a)
        moved = 0
        for dev in sorted(by_dev):
            prev: int | None = None
            for a in sorted(by_dev[dev], key=lambda x: (x["raised_at"], x["id"])):
                if prev is not None:
                    need = prev + int(rng.lognormal(mean=math.log(median_ms), sigma=sigma))
                    if a["raised_at"] < need:
                        dur = None if a["cleared_at"] is None else a["cleared_at"] - a["raised_at"]
                        a["raised_at"] = need
                        if dur is not None:
                            a["cleared_at"] = need + dur
                        moved += 1
                prev = a["raised_at"]
        return moved

    def raise_(self, resource: str, mechanism: str, at: int, cleared: int | None, scenario: str,
               fault: Fault | None, hops: int, severity: str, service_affecting: bool = False) -> None:
        a = self.cfg["alarms"]
        if fault is not None and self.rng.random() > a["emit_prob"].get(mechanism, 1.0):
            return                                                       # the alarm that never came
        at += self.skew.get(self.device_of(resource), 0)
        kind = fault.fault_type if mechanism == "root" and fault else mechanism
        cause = self.causes[kind][int(self.rng.integers(len(self.causes[kind])))]
        base = {"alarm_type": ALARM_TYPE.get(kind, "qualityOfServiceAlarm"),
                "perceived_severity": severity, "probable_cause": cause,
                "specific_problem": specific_problem_for(cause),
                "service_affecting": service_affecting, "scenario": scenario, "resource": resource,
                # Ground truth. pack.py strips it before the alarm is emitted; it reaches the
                # dataset only through CAUSED.via and ground_truth.jsonl.
                "mechanism": mechanism}
        times = [(at, cleared)]
        if mechanism in ("loss-of-signal", "background") and self.rng.random() < a["flap_prob"]:
            t, flaps = at, []                                            # raise/clear cycles first
            for _ in range(int(self.rng.integers(2, 5))):
                up = t + int(self.rng.integers(5_000, 90_000))
                flaps.append((t, up))
                t = up + int(self.rng.integers(5_000, 60_000))
            times = flaps + [(t, cleared)]
        if self.rng.random() < a["duplicate_prob"]:
            times.append((at + int(self.rng.integers(500, 15_000)), cleared))
        for raised, clr in times:
            # A flap cycle walks the raise forward through up to four up/down pairs, and a
            # duplicate lands up to 15 s later, while the clear is fixed at the fault's
            # repair. A cycle that lands after the repair is an alarm that never happened:
            # the fault was fixed before it would have recurred. Emitting it anyway shipped
            # 11 alarms whose cleared_at preceded their raised_at.
            if clr is not None and raised > clr:
                continue
            aid = f"alm:{scenario}:{self.k}"
            self.k += 1
            self.alarms.append({**base, "id": aid, "raised_at": int(raised),
                                "cleared_at": None if clr is None else int(clr)})
            if fault is not None:
                self.caused.append((fault.id, aid, mechanism, hops))


def lognormal_ms(rng, median_ms: float, sigma: float) -> int:
    return int(rng.lognormal(mean=math.log(median_ms), sigma=sigma))


def simulate(net: Network, faults: list[Fault], scenario: str, window: tuple[int, int],
             noc_pop: str, af: AlarmFactory) -> Outcome:
    """Raise the alarms these faults cause inside the window, plus background."""
    rng, a = af.rng, af.cfg["alarms"]
    t0 = min(f.onset for f in faults)
    out = effect(net, faults, noc_pop)
    end = window[1]

    def clear(f: Fault) -> int | None:
        c = f.repair + int(rng.integers(5_000, 120_000))
        return c if c <= end else None

    for f in faults:
        reported = [r for r in f.resources if not r.startswith("pop:") and (
            r not in net.device_cards or f.fault_type in ("software_crash", "config_change"))]
        for r in reported:                                   # the root, when it can still speak
            af.raise_(r, "root", f.onset + lognormal_ms(rng, 1_500, 0.6), clear(f), scenario, f, 0,
                      SEVERITY["root"])
    owner = {}                                               # which fault each effect belongs to
    for f in faults:
        solo = effect(net, [f], noc_pop)
        for p in solo.los_ports:
            owner.setdefault(("los", p), f)
        for d in solo.isolated:
            owner.setdefault(("iso", d), f)
        for s in solo.services_out:
            owner.setdefault(("svc", s["id"]), f)
    for p in sorted(out.los_ports):
        f = owner.get(("los", p), faults[0])
        af.raise_(p, "loss-of-signal", f.onset + lognormal_ms(rng, a["los_delay_ms"], 0.5), clear(f),
                  scenario, f, 1, SEVERITY["loss-of-signal"])
    for d in sorted(out.isolated):
        f = owner.get(("iso", d), faults[0])
        poll = int(rng.integers(0, a["poll_interval_ms"]))
        af.raise_(d, "isolation", f.onset + poll + lognormal_ms(rng, 5_000, 0.5), clear(f),
                  scenario, f, 2, SEVERITY["isolation"], service_affecting=True)
    dead = out.down_devices | out.isolated
    for s in out.services_out:
        f = owner.get(("svc", s["id"]), faults[0])
        ends = list(s["live_ends"])                          # only an end that can still report
        if s["kind"] == "mobile-data":                       # the core sees the sessions drop
            ends += [d for d, _ in s["deps"][1:] if d not in dead][:1]
        for d in ends:
            af.raise_(d, "service", f.onset + lognormal_ms(rng, a["service_delay_ms"], 0.6), clear(f),
                      scenario, f, 3, SEVERITY["service"], service_affecting=True)
    for d in sorted(x for x in dead if net.device_role[x] == "ran"):
        f = owner.get(("iso", d)) or next((g for g in faults if d in effect(net, [g], noc_pop).down_devices),
                                          faults[0])
        pop = net.device_pop[d]
        near = [x for p in [pop] + sorted(net.pop_adj.get(pop, [])) for x in net.pop_devices[p]["ran"]
                if x != d and x not in dead]
        for x in near[: a["ran_neighbours"]]:
            # A live site does not notice a dead neighbour the moment it dies: the
            # degradation is detected on that site's own measurement period, so the offset
            # into the period -- drawn once per site -- is what spreads the storm. Without
            # it every neighbour alarm is drawn from one lognormal around a common onset,
            # and 0.205 of the stream's inter-arrival gaps came out under one second
            # against 0.019 in real operator alarms; 283 of those 332 sub-second gaps were
            # neighbour alarms. The cells on one site still report together, because they
            # report through that site.
            period = a["neighbour_report_period_ms"]
            report = int(rng.integers(0, period)) if period else 0
            for _ in range(int(rng.integers(1, a["cells_per_ran"] + 1))):
                af.raise_(x, "neighbour", f.onset + report + lognormal_ms(rng, 20_000, 0.6),
                          clear(f), scenario, f, 2, SEVERITY["neighbour"])
    if any(f.fault_type in ("fibre_cut", "power_loss", "device_failure") for f in faults):
        cores = [d for d, r in net.device_role.items() if r == "core" and d not in out.down_devices]
        for d in rng.choice(cores, size=min(len(cores), int(rng.integers(0, 4))), replace=False):
            f = faults[0]
            af.raise_(str(d), "congestion", f.onset + lognormal_ms(rng, 120_000, 0.7),
                      clear(f), scenario, f, 3, SEVERITY["congestion"])
    background(af, scenario, (t0 - a["background_lead_ms"], end))
    return out


def background(af: AlarmFactory, scenario: str, window: tuple[int, int]) -> None:
    rng, a, net = af.rng, af.cfg["alarms"], af.net
    hours = (window[1] - window[0]) / HOUR
    ports = sorted(net.peer)
    devices = sorted(net.device_role)
    for _ in range(int(rng.poisson(a["background_per_hour"] * hours))):
        r = devices[int(rng.integers(len(devices)))] if rng.random() < 0.4 else ports[int(rng.integers(len(ports)))]
        at = int(rng.integers(window[0], window[1]))
        dur = lognormal_ms(rng, 10 * MIN, 1.0)
        af.raise_(r, "background", at, at + dur if at + dur <= window[1] else None, scenario, None, -1,
                  "warning" if rng.random() < 0.7 else "minor")


def pick_fault(net: Network, rng, fault_type: str, onset: int, repair_median_ms: int, fid: str,
               scenario: str, pop: str | None = None) -> Fault:
    pops = sorted(net.pop_devices)
    pop = pop or pops[int(rng.integers(len(pops)))]
    roles = net.pop_devices[pop]

    def choose(xs):
        return xs[int(rng.integers(len(xs)))]
    if fault_type == "card_failure":
        dev = choose(roles["aggregation"] + roles["core"])
        res = [choose(net.device_cards[dev])]
    elif fault_type in ("device_failure", "software_crash", "config_change"):
        res = [choose(roles["aggregation"] + roles["core"] + roles["access"])]
    elif fault_type == "port_failure":
        dev = choose(roles["aggregation"])
        res = [choose([p for c in net.device_cards[dev] for p in net.card_ports[c] if p in net.peer])]
    elif fault_type == "fibre_cut":
        mine = [bb for bb in net.backbone if net.device_pop[net.port_device(bb[0])] == pop]
        res = list(choose(mine)) if mine else list(choose(net.backbone))
    elif fault_type == "power_loss":
        res = [pop]
    else:
        raise ValueError(f"unknown fault type {fault_type!r}")
    ports = []
    if fault_type == "config_change":                   # half the cabled interfaces misconfigured
        cabled = [p for c in net.device_cards[res[0]] for p in net.card_ports[c] if p in net.peer]
        ports = [p for p in cabled if rng.random() < 0.5] or cabled[:1]
    repair = onset + lognormal_ms(rng, repair_median_ms, 0.6)
    return Fault(fid, fault_type, res, onset, repair, scenario, ports=ports)
