"""Incidents, changes, runbooks and ticket notes: the text side that retrieval searches.

The NOC's view is imperfect on purpose. An incident groups the alarms the NOC tied to it:
some caused alarms are missed or land in a duplicate incident, and some background alarms
are wrongly attached. A fault that raised no alarm never becomes an incident.

History and evaluation are kept apart. History incidents carry their resolution: root-cause
resource, runbook used, full engineer notes. They are the corpus "similar past incidents"
retrieves from. Evaluation incidents carry only the opening note (symptoms), never the
cause, so retrieval is never scored on text that states the answer.

Text is templated. An optional LLM pass for more natural variation is planned
(samyama-synthetic-data-generator#5); the default build needs no API key.
"""

from __future__ import annotations

from typing import Any

from .faults import MIN, Fault

ACTIONS = {
    "card_failure": ("replaced line card {res} on {dev}", "card_replacement"),
    "device_failure": ("replaced the failed chassis {dev} and restored config from backup", "hardware_replacement"),
    "software_crash": ("reloaded {dev}; raised a vendor case for the crash dump", "software_restart"),
    "power_loss": ("site power restored at {pop}; devices came back on mains", "power_restored"),
    "port_failure": ("replaced the optic on {res}", "optic_replacement"),
    "fibre_cut": ("field team spliced the cut fibre on backbone link {res}", "fibre_repair"),
    "config_change": ("rolled back change {change} on {dev}", "change_rollback"),
}
STEPS = {
    "equipmentAlarm": ["Check card and chassis LEDs via the element manager.",
                       "Compare the inventory serial with the alarmed slot.",
                       "If the card is unresponsive, reseat it; if it stays down, replace it."],
    "communicationsAlarm": ["List ports in loss of signal on both ends of the link.",
                            "If both ends of one link are dark, suspect the fibre; dispatch a field team.",
                            "If one device has many dark ports, suspect that device or its card."],
    "qualityOfServiceAlarm": ["Check utilisation on the service path.",
                              "Look for a failure upstream that forced a reroute.",
                              "Raise the incident priority if a gold SLA is affected."],
    "processingErrorAlarm": ["Check recent changes on the device.",
                             "Collect logs and the crash dump, if any.",
                             "Roll back the last change, or reload the device."],
    "environmentalAlarm": ["Check site power and cooling telemetry.",
                           "Confirm with site facilities; dispatch if mains power is lost."],
}


def runbooks(alarm_types: list[str], kinds: list[str]) -> list[dict[str, Any]]:
    out = []
    for at in sorted(alarm_types):
        for kind in sorted(kinds):
            steps = STEPS.get(at, ["Check the alarmed resource and its neighbours."])
            out.append({"id": f"rb:{at}:{kind}", "alarm_type": at, "resource_kind": kind,
                        "title": f"{at} on a {kind}",
                        "text": f"Runbook for {at} raised on a {kind}. " + " ".join(
                            f"{i + 1}. {s}" for i, s in enumerate(steps))})
    return out


def incidents(fault: Fault, alarms: list[dict[str, Any]], caused: set[str], background: list[dict[str, Any]],
              split: str, net, cfg: dict[str, Any], rng, change_id: str | None) -> list[dict[str, Any]]:
    """The NOC's incidents for one fault: [{incident fields, alarms: [ids], runbook}]."""
    mine = sorted((a for a in alarms if a["id"] in caused), key=lambda a: a["raised_at"])
    if not mine:
        return []
    t = cfg["tickets"]
    grouped, stray = [], []
    for a in mine:
        (stray if rng.random() < t["misgroup_prob"] else grouped).append(a["id"])
    noise = [b["id"] for b in background if rng.random() < t["background_attach_prob"]
             and abs(b["raised_at"] - mine[0]["raised_at"]) < 30 * MIN]
    first = mine[0]
    opened = first["raised_at"] + int(rng.integers(2 * MIN, 15 * MIN))
    resolved = fault.repair + int(rng.integers(5 * MIN, 30 * MIN))
    affecting = any(a["service_affecting"] for a in mine)
    root_res = fault.resources[0]
    dev = root_res if root_res in net.device_cards else (
        net.card_device.get(root_res) or (net.port_device(root_res) if root_res in net.port_card else root_res))
    pop = net.device_pop.get(dev, root_res)
    action, code = ACTIONS[fault.fault_type]
    symptoms = (f"{len(mine)} alarms from {len({a['resource'] for a in mine})} resources, first "
                f"{first['alarm_type']} ({first['probable_cause']}) on {first['resource']}.")
    base = {"opened_at": opened, "priority": "P1" if affecting else ("P2" if len(mine) > 5 else "P3"),
            "scenario": fault.scenario, "split": split, "synthetic": True}
    inc = {**base, "id": f"inc:{fault.id}", "alarms": grouped + noise,
           "opening_note": f"Opened by NOC. {symptoms}",
           "runbook": f"rb:{first['alarm_type']}:{kind_of(net, first['resource'])}"}
    if split == "history":
        inc.update(resolved_at=resolved, resolution_code=code, root_cause_resource=root_res,
                   resolution_note=(f"Root cause: {fault.fault_type.replace('_', ' ')} at {root_res}. "
                                    + action.format(res=root_res, dev=dev, pop=pop, change=change_id)
                                    + f". Service restored after {(resolved - fault.onset) // MIN} min."))
    out = [inc]
    if stray and rng.random() < t["duplicate_incident_prob"]:
        out.append({**base, "id": f"inc:{fault.id}:dup", "alarms": stray, "priority": "P3",
                    "opening_note": f"Opened by NOC. {len(stray)} alarms, not yet linked to another incident.",
                    "runbook": None})
    return out


def kind_of(net, r: str) -> str:
    if r in net.device_role:
        return net.device_role[r]
    if r in net.card_device:
        return "card"
    return "port"
