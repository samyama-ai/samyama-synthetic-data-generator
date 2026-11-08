"""Telecom pack: determinism, stream independence, binding, ground truth, validation."""

import copy
import itertools
import json
import statistics
from pathlib import Path

import pytest
import yaml

from sdg.ontology import BindingError, bind
from sdg.packs.telecom.pack import SPEC, generate
from sdg.validate import validate

ROOT = Path(__file__).resolve().parent
BUILD = ROOT / "fixtures" / "ontology-build"


def cfg(**over):
    c = yaml.safe_load((ROOT.parent / "configs" / "telecom" / "small.yaml").read_text())
    c["topology"] = str(ROOT / "fixtures" / "Abilene.graphml")
    c["scenarios"] = [
        {"split": "history", "kind": "single", "count": 4, "fault_types": {"card_failure": 1, "port_failure": 1}},
        {"split": "eval", "kind": "single", "count": 4, "fault_types": {"device_failure": 1, "config_change": 1}},
        {"split": "eval", "kind": "storm", "count": 2, "fault_types": {"power_loss": 1}},
        {"split": "eval", "kind": "overlap", "count": 2, "fault_types": {"card_failure": 1, "software_crash": 1}},
    ]
    c["network"]["enterprise_vpns"] = 5
    c.update(over)
    return c


def run(tmp_path, name, seed=7, c=None):
    out = tmp_path / name
    generate(c or cfg(), seed, BUILD, out)
    return out


def test_same_seed_same_bytes_and_a_new_seed_differs(tmp_path):
    a, b, c = run(tmp_path, "a"), run(tmp_path, "b"), run(tmp_path, "c", seed=8)
    for f in ("nodes.jsonl", "edges.jsonl", "alarms.stream.jsonl", "ground_truth.jsonl"):
        assert (a / f).read_bytes() == (b / f).read_bytes(), f
    assert (a / "nodes.jsonl").read_bytes() != (c / "nodes.jsonl").read_bytes()


def test_alarm_knobs_leave_the_inventory_untouched(tmp_path):
    c2 = cfg()
    c2["alarms"] = copy.deepcopy(c2["alarms"])
    c2["alarms"]["background_per_hour"] = 9
    a, b = run(tmp_path, "a"), run(tmp_path, "b", c=c2)

    def inventory(d):
        return [line for line in (d / "nodes.jsonl").open()
                if json.loads(line)["label"] in ("PoP", "Device", "Card", "Port", "Service")]
    assert inventory(a) == inventory(b)
    assert (a / "alarms.stream.jsonl").read_bytes() != (b / "alarms.stream.jsonl").read_bytes()


def test_unknown_ontology_term_stops_the_run():
    spec = copy.deepcopy(SPEC)
    spec["labels"]["Alarm"] = ["noria:NoSuchClass"]
    with pytest.raises(BindingError, match="NoSuchClass"):
        bind(BUILD, spec)


def test_enums_come_from_the_vocabularies():
    b = bind(BUILD, SPEC)
    assert b.values("perceived_severity") == ["indeterminate", "warning", "minor", "major", "critical"]
    assert "equipmentAlarm" in b.values("alarm_type")
    assert "lossOfSignal" in b.values("probable_cause")


def test_dataset_validates_and_ground_truth_holds(tmp_path):
    out = run(tmp_path, "a")
    rep = validate(out)
    assert rep["summary"]["conformance_errors"] == 0, rep["errors"][:5]
    truth = [json.loads(line) for line in (out / "ground_truth.jsonl").open()]
    assert len(truth) == 14                                  # 12 scenarios, 2 of them two-fault overlaps
    assert all(t["change"] for t in truth if t["fault_type"] == "config_change")
    nodes = {json.loads(line)["id"]: json.loads(line) for line in (out / "nodes.jsonl").open()}
    inc = [n for n in nodes.values() if n["label"] == "Incident"]
    assert all("root_cause_resource" not in n and "resolution_note" not in n for n in inc if n["split"] == "eval")
    assert any("root_cause_resource" in n for n in inc if n["split"] == "history")
    m = json.loads((out / "manifest.json").read_text())
    assert set(m["inputs"]["ontology_builds"]) == {f"{v}/{f}" for v in SPEC["vocabularies"]
                                                   for f in ("nodes.jsonl", "edges.jsonl")}


def test_no_alarm_carries_its_role_in_the_fault(tmp_path):
    """The mechanism is ground truth, so it may appear on the Fault edge and nowhere else.

    It used to ride on the alarm as `specific_problem`. That shipped the answer: an alarm
    reading `specific_problem == "root"` sat on a ground-truth root resource in every eval
    incident that had one, so a one-line method scored as well as the best baseline in the
    paper without doing any reasoning. This asserts both halves -- the alarm no longer says
    it, and ground truth still does -- because dropping the field without moving it would
    make the dataset unscorable instead of fixing it.
    """
    out = run(tmp_path, "a")
    nodes = [json.loads(line) for line in (out / "nodes.jsonl").open()]
    alarms = [n for n in nodes if n["label"] == "Alarm"]
    roles = {"root", "neighbour", "isolation", "service", "loss-of-signal", "background"}
    for a in alarms:
        assert "mechanism" not in a
        assert not (roles & {v for v in a.values() if isinstance(v, str)}), a
    assert {a["specific_problem"] for a in alarms} - {""}, "every alarm still has an alarm text"
    edges = [json.loads(line) for line in (out / "edges.jsonl").open()]
    via = {e["dst"]: e["via"] for e in edges if e["rel"] == "CAUSED"}
    truth = [json.loads(line) for line in (out / "ground_truth.jsonl").open()]
    assert via == {a: m for t in truth for a, m in t["alarm_mechanisms"].items()}
    assert "root" in set(via.values())


def test_the_alarm_text_says_no_more_than_the_probable_cause(tmp_path):
    """specific_problem is a function of probable_cause, so it cannot carry side information.

    One value per cause is what makes the leak impossible to reintroduce by accident: if the
    text ever varied with anything else, that something else would be readable off the alarm.
    """
    out = run(tmp_path, "a")
    alarms = [json.loads(line) for line in (out / "nodes.jsonl").open()]
    seen = {}
    for a in (x for x in alarms if x["label"] == "Alarm"):
        seen.setdefault(a["probable_cause"], set()).add(a["specific_problem"])
    assert all(len(v) == 1 for v in seen.values()), {k: v for k, v in seen.items() if len(v) > 1}
    assert len(seen) > 1


def test_an_override_changes_one_thing_and_a_typo_stops_the_run(tmp_path):
    """`--set` exists so an ablation is a difference of one knob, not of two files.

    The CLI parses it, so this covers the resolution rule rather than argparse: a dotted key
    must already exist, because a typo that silently creates a new key produces a run that
    looks like an ablation and is the baseline.
    """
    from sdg.cli import main as cli

    onto = tmp_path / "ontologies"              # the CLI looks for <--ontologies>/build
    onto.mkdir()
    (onto / "build").symlink_to(BUILD)
    cfgfile = tmp_path / "c.yaml"
    cfgfile.write_text(yaml.safe_dump(cfg()))
    base, abl = tmp_path / "base", tmp_path / "abl"
    args = ["generate", str(cfgfile), "--ontologies", str(onto)]
    assert cli(args + ["--out", str(base)]) == 0
    assert cli(args + ["--out", str(abl), "--set", "alarms.background_per_hour=0"]) == 0

    def alarms(d):
        return [json.loads(line) for line in (d / "nodes.jsonl").open()
                if json.loads(line)["label"] == "Alarm"]
    assert len(alarms(abl)) < len(alarms(base))
    m = json.loads((abl / "manifest.json").read_text())
    assert m["config"]["alarms"]["background_per_hour"] == 0      # the manifest records it
    with pytest.raises(SystemExit):
        cli(args + ["--out", str(tmp_path / "x"), "--set", "alarms.no_such_knob=0"])
    with pytest.raises(SystemExit):
        cli(args + ["--out", str(tmp_path / "y"), "--set", "alarms.background_per_hour"])


def alarm_nodes(d):
    return [json.loads(line) for line in (d / "nodes.jsonl").open()
            if json.loads(line)["label"] == "Alarm"]


def raise_gaps(d):
    ts = sorted(json.loads(line)["t"] for line in (d / "alarms.stream.jsonl").open()
                if json.loads(line)["event"] == "raise")
    return [(b - a) / 1000.0 for a, b in itertools.pairwise(ts)]


def test_an_alarm_never_clears_before_it_is_raised(tmp_path):
    """A flapping alarm used to outlive its own clear.

    `raise_` walks a flapping alarm's raise forward through up to four up/down cycles, and
    a duplicate lands up to 15 s later, while the clear is fixed at the fault's repair. A
    cycle that lands after the repair is an alarm that never happened; emitting it anyway
    put 11 alarms with `cleared_at < raised_at` into the dataset published on 2026-10-21,
    and nothing checked. Every cycle flaps here, so the overrun is certain.
    """
    c = cfg()
    c["alarms"] = copy.deepcopy(c["alarms"])
    c["alarms"]["flap_prob"] = 1.0
    c["alarms"]["duplicate_prob"] = 1.0
    c["repair_median_ms"] = 60_000
    out = run(tmp_path, "flappy", c=c)
    al = alarm_nodes(out)
    assert al, "no alarms, so the assertion below could not fail"
    assert [a["id"] for a in al
            if a["cleared_at"] is not None and a["cleared_at"] < a["raised_at"]] == []
    # And the validator fails on it, so a dataset carrying one cannot pass unnoticed.
    assert validate(out)["summary"]["conformance_errors"] == 0


def test_one_element_sends_its_notifications_one_at_a_time(tmp_path):
    """Two notifications cannot leave one element at the same millisecond."""
    out = run(tmp_path, "serial")
    on = {}
    for line in (out / "edges.jsonl").open():
        e = json.loads(line)
        if e["rel"] == "RAISED_ON":
            on[e["src"]] = e["dst"]

    def device(resource):
        parts = resource.split(":")
        return ":".join(parts[:4]) if parts[0] == "dev" else resource

    seen = set()
    for a in alarm_nodes(out):
        key = (device(on[a["id"]]), a["raised_at"])
        assert key not in seen, f"two alarms left {key[0]} at the same instant"
        seen.add(key)


def test_a_live_site_detects_a_dead_neighbour_on_its_own_period(tmp_path):
    """The storm's sub-second clustering was one lognormal around a common onset.

    With the period at zero every neighbour alarm is drawn from that one cloud, which is
    what the published dataset did: 0.205 of its inter-arrival gaps were under a second
    against 0.019 in real operator alarms. Each live site now draws its own offset into its
    measurement period. The assertion is on the knob's effect, not on a particular rate.
    """
    def neighbour_spans(d):
        """Seconds between the first and last neighbour alarm of each storm."""
        at = {a["id"]: a["raised_at"] for a in alarm_nodes(d)}
        by_scenario = {}
        for line in (d / "edges.jsonl").open():
            e = json.loads(line)
            if e["rel"] == "CAUSED" and e.get("via") == "neighbour":
                by_scenario.setdefault(e["src"], []).append(at[e["dst"]])
        return [(max(ts) - min(ts)) / 1000.0 for ts in by_scenario.values() if len(ts) >= 5]

    c0, c1 = cfg(), cfg()
    c0["alarms"] = copy.deepcopy(c0["alarms"])
    c0["alarms"]["neighbour_report_period_ms"] = 0
    cloud, spread = run(tmp_path, "cloud", c=c0), run(tmp_path, "spread", c=c1)
    one, many = neighbour_spans(cloud), neighbour_spans(spread)
    assert one and many, "no storm raised five neighbour alarms, so nothing is measured"
    # A storm drawn from one lognormal around the onset spans tens of seconds; drawn with a
    # per-site offset into a 15-minute period it spans minutes.
    assert max(one) < c1["alarms"]["neighbour_report_period_ms"] / 1000.0
    assert statistics.median(many) > 5 * statistics.median(one)
