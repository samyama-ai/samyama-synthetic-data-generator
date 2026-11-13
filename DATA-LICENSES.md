# Data licences

`LICENSE` in this repository covers the **generator code**. It says nothing about the
vocabularies and topologies this generator reads, nor about the datasets it writes. That
gap is what this file closes.

Each row records what the source's **own terms** say, with where we read them and when.
Where something is unsettled it says so rather than guessing: an unverified licence
written down as fact is worse than the silence it replaces.

## What the generator reads

| Source | What we use | What its terms say | Checked |
|---|---|---|---|
| [NORIA-O 0.3](https://github.com/Orange-OpenSource/noria-ontology) | 6 class IRIs as label bindings, and the `ProbableCause` SKOS scheme (21 of its 173 concepts appear as alarm values) | BSD-4-Clause, copyright 2022-2023 Orange. Redistribution in source or derived form must retain the copyright notice, the conditions and the disclaimer. **Clause 3 additionally requires material mentioning features or use of the software to carry an acknowledgement**, and clause 4 forbids using Orange's name to endorse derived work. | 2026-10-21 |
| [RFC 8632](https://www.rfc-editor.org/rfc/rfc8632) (alarm management YANG) | the `severity` typedef, bound to `perceived_severity` | Simplified BSD, IETF Trust. Code components may be redistributed with the notice carried forward. | 2026-10-21 |
| [RFC 8345](https://www.rfc-editor.org/rfc/rfc8345) (network topology YANG) | node and termination-point classes, bound to Device and Port | Simplified BSD, IETF Trust. | 2026-10-21 |
| [TMF642 Alarm Management API](https://www.tmforum.org/oda/open-apis/) | the `AlarmType` enum, bound to `alarm_type` | Apache-2.0, TM Forum. | 2026-10-21 |
| [Internet Topology Zoo](http://www.topology-zoo.org/) | the backbone graphs (GÉANT 2012, Abilene, Cogentco): PoPs and the links between them | CC BY 4.0. Redistribution and adaptation permitted with attribution. | 2026-10-21 |
| [UCI Adult (Census Income)](https://archive.ics.uci.edu/dataset/2/adult) | demo only: downloaded by `examples/adult_e2e/fetch.sh`, never vendored; the repo ships a DP-fitted spec of noisy parameters, not rows | CC BY 4.0 (Becker & Kohavi, 1996). Redistribution and adaptation permitted with attribution. | 2026-11-13 |

`tests/fixtures/ontology-build/` holds a **derived redistribution** of the first four, as
flattened JSONL. The NORIA-O directory therefore carries `LICENSE.txt` and `NOTICE.md`,
because clauses 1 and 2 require it. It did not until 2026-10-21; that omission is recorded
in the notice rather than quietly fixed.

## What the generator writes

A generated dataset is **not** a redistribution of the inputs above. It carries their
*term names* — `noria:EventRecord`, `lossOfSignal`, `communicationsAlarm` — and none of
their files. The one real component is the Topology Zoo backbone it is built on.

| | |
|---|---|
| The generated data | **CC BY 4.0**, which carries Topology Zoo's own terms for the backbone |
| The generator and scoring code | **Apache-2.0** |
| Synthetic share | 0.9969 of nodes carry `synthetic: true`; the rest is the public backbone |
| Personal data | none. Customer nodes carry a market segment and nothing else — no names, no contacts, no identifiers |

### The acknowledgement NORIA-O's licence requires

> This product includes software developed by Orange.

That sentence is an obligation of NORIA-O's BSD-4-Clause licence and travels with any
material describing use of this generator. It is a statement about the ontology, not about
any commercial relationship, and Orange does not endorse this work. The reasoning behind
treating a dataset of term names this way is in `telecom-kg/LICENCE-REVIEW-NORIA-O.md`.

## How to read the "generated data" line

A dataset built from several sources carries **all** of their terms at once, and the
restrictive ones win. Here the inputs are CC BY 4.0, two Simplified BSD licences, Apache-2.0
and one BSD-4-Clause — none of them non-commercial, none share-alike — so the result is
attribution-only. The advertising clause is the single obligation that does not simply
disappear into the output, which is why it has a section of its own above.

## If you redistribute

- Keep the attributions named above with the data, and carry the Orange acknowledgement.
- State which generator version and seed produced your copy: `manifest.json` records both,
  and the same seed reproduces the files byte for byte.
- Re-read the terms pages. Licences change, and the dates in this table are when we last
  looked.
