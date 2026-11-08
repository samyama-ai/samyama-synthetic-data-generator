# samyama-synthetic-data-generator

Seeded, labelled, ontology-bound synthetic data compiler with $O(1)$-memory referential integrity and causal ground truth. Built in **Rust** (`sdg-rs` / `samyama_sdg`) and **Python** (`sdg`) around two complementary layers:

1. **Layer 1 — Multi-Frontend Declarative Schema & Data Dictionary Compiler (`configs/schemas/`):**
   - **Three Schema Frontends -> Single Typed IR (`CompiledSchema`):**
     1. **GraphQL SDL (`.graphql`):** `type Entity @count(N) @edge(from: "...", to: "...", type: "...") { ... }`
     2. **SQL DDL (`.sql`):** `CREATE TABLE ... PRIMARY KEY ... REFERENCES ... CHECK (...)` with `-- @count(N)` / `-- @after(...)` / `-- @fk(...)` comment directives (`configs/schemas/health_clinic.sql`).
     3. **OWL / RDFS / SKOS Turtle (`.ttl`):** Compiles `owl:Class`, `owl:ObjectProperty`, `owl:DatatypeProperty`, `rdfs:subClassOf`, and `skos:ConceptScheme` directly into synthetic graphs and tables (`configs/schemas/clinical_ontology.ttl`).
   - **Rich Directives & Intra-Row Formulas:** `@count(N)`, `@pk(kind: "uuid"|"ulid"|"prefix")`, `@fk(dist: "uniform"|"zipf", s: 1.2)`, `@after(field: "parent.date", min_days: 0, max_days: 30)`, `@derived(formula: "balance - holds")`, `@edge(from: "fromAccount", to: "toAccount", type: "TRANSFERRED")`, `@constraint(pattern: "...", min: ..., max: ...)`, and `@weights(values: [...])` (`configs/schemas/banking_graph.graphql`).
   - **Global Attribute Data Dictionary (`configs/schemas/dictionary.yaml`):** Bind attribute names (`email`, `phone`, `icd10`, `cptCode`, `ndcCode`, `sku`, `ipv4`, `dob`) to compiled regex ASTs, parametric distributions (`@dist`), enumerations, or `samyama-ontologies` vocabulary builds (`@ontology`).
   - **$O(1)$-Memory Index-Bijective Foreign Keys:** Because primary keys and temporal dates are pure bijective functions $f_{\text{PK}}(s, E, i)$ of the seed, entity type, and integer row index $i \in [0, N_E)$, child entities resolve foreign keys and parent dates in $O(1)$ RAM without storing parent ID tables in memory.
   - **Dual Output Modes:** Emits relational CSVs (`<Entity>.csv`) and/or Property Graph files (`nodes.jsonl`, `edges.jsonl`, `binding.json`, `manifest.json`).
2. **Layer 2 — Domain Causal Process & Ground-Truth Packs (`configs/telecom/*.yaml`):**
   - **Vocabulary-bound:** Binds every class and enumeration to samyama-ontologies (term builds are vendored under `tests/fixtures/ontology-build/`) builds (`NORIA-O`, `RFC 8632`, `TMF642`) with SHA-256 provenance (`binding.json`).
   - **Causal ground truth (`ground_truth.jsonl`):** Simulates fault propagation over real Internet Topology Zoo backbones (`CC BY 4.0`) and emits machine-scorable root-cause labels alongside noisy operational streams (`alarms.stream.jsonl`).

## Quickstart

### Rust Engine (`sdg-rs` — 1M+ records in `< 0.2 s`, $O(1)$ memory)

```bash
# Build release binary
cargo build --release

# Compile & stream 1,000,000 relational + graph records (scale=1000) from a GraphQL schema
./target/release/sdg-rs schema configs/schemas/health_clinic.graphql \
  --dictionary configs/schemas/dictionary.yaml \
  --scale 1000 --format both --out out/hc-1m

# Validate PK uniqueness, 100% FK referential integrity, regex/enum conformance, and @after ordering
./target/release/sdg-rs validate-schema out/hc-1m

# Run multi-scale throughput & peak RSS benchmarks
./target/release/sdg-rs bench configs/schemas/health_clinic.graphql \
  --dictionary configs/schemas/dictionary.yaml \
  --scales 1,10,100,1000,5000 --format csv --out out/bench_hc.json

# Run the Telecom NOC causal alarm-storm generator & validator in native Rust
./target/release/sdg-rs generate configs/telecom/small.yaml --out out/telecom-small-rs
./target/release/sdg-rs validate out/telecom-small-rs
```

### Python CLI (`sdg`)

```bash
uv run sdg generate configs/telecom/small.yaml --out out/telecom-small     # ~2 s
uv run sdg validate out/telecom-small                                      # writes realism.json
```

## Declarative GraphQL Schema Compiler (`configs/schemas/`)

Any multi-entity relational or property-graph dataset can be specified in ~50 lines of GraphQL SDL without writing domain code. Example (`configs/schemas/health_clinic.graphql`):

```graphql
enum ClaimStatus { SUBMITTED ADJUDICATED PAID DENIED }

type Patient @count(n: 200) @pk(kind: "prefixed", prefix: "PAT") @ontology(vocab: "health-core", class: "Patient") {
  id: ID!
  ssn: SSN! @regex(pattern: "[0-9]{3}-[0-9]{2}-[0-9]{4}")
  firstName: String!
  lastName: String!
  gender: Gender!
  dob: Date! @dist(kind: "date", start: "1940-01-01", end: "2005-12-31")
  email: Email!
  phone: Phone!
}

type Doctor @count(n: 50) @pk(kind: "prefixed", prefix: "DOC") @ontology(vocab: "health-core", class: "Physician") {
  id: ID!
  npi: String! @regex(pattern: "[1-9][0-9]{9}")
  specialty: Specialty!
}

type Appointment @count(n: 400) @pk(kind: "prefixed", prefix: "APT") @ontology(vocab: "health-core", class: "Encounter") {
  id: ID!
  patient: Patient! @fk(dist: "uniform", edge: "FOR_PATIENT")
  doctor: Doctor! @fk(dist: "zipf", s: 1.15, edge: "ATTENDED_BY")
  appointmentDate: Date! @after(field: "patient.dob", min_days: 6570, max_days: 25550)
}
```

### Measured Throughput & Memory (`sdg-rs` release build, seed `20261019`)

| Schema | Format | Scale | Total Rows | Total Edges | Wall Time | Throughput (`rows/s`) | Peak RSS |
|---|---|---:|---:|---:|---:|---:|---:|
| `health_clinic.graphql` (5 tables) | CSV | `1x` | `1,000` | `1,150` | `2.15 ms` | `465,084` | `7.7 MiB` |
| `health_clinic.graphql` (5 tables) | CSV | `100x` | `100,000` | `115,000` | `27.95 ms` | `3,577,645` | `18.0 MiB` |
| `health_clinic.graphql` (5 tables) | CSV | `1000x` | `1,000,000` | `1,150,000` | **`172.43 ms`** | **`5,799,436`** | **`35.3 MiB`** |
| `health_clinic.graphql` (5 tables) | CSV | `5000x` | `5,000,000` | `5,750,000` | **`808.56 ms`** | **`6,183,852`** | **`35.1 MiB`** |
| `health_clinic.graphql` (5 tables) | CSV + Graph JSONL | `1000x` | `1,000,000` | `1,150,000` | **`438.70 ms`** | **`2,279,484`** | `125.7 MiB` |
| `ecommerce.graphql` (4 tables) | CSV | `1000x` | `1,000,000` | `1,120,000` | **`128.79 ms`** | **`7,764,466`** | **`31.4 MiB`** |
| `university.graphql` (4 tables) | CSV | `1000x` | `1,000,000` | `1,050,000` | **`147.15 ms`** | **`6,795,932`** | **`31.1 MiB`** |

- **$O(1)$ Streaming Memory:** Peak RSS between `1,000,000` rows (`35.3 MiB`) and `5,000,000` rows (`35.1 MiB`) is flat because parent tables are never buffered in RAM.
- **100% Referential, Regex & Temporal Conformance:** `sdg-rs validate-schema` checks all `1,000,000` rows, `1,150,000` foreign keys, `450,000` regex attributes, and `900,000` multi-hop `@after` constraints (`Patient.dob <= Appointment.appointmentDate <= Diagnosis.recordedDate <= Claim.filedDate`) with **0 conformance errors** in `399 ms`.

## Principles

- **Seeded and reproducible.**
  - Output is a function of (pack version, config, seed). The same seed gives byte-identical files (tested).
  - Each stage draws from its own named random stream. Changing the alarm settings leaves the inventory untouched (tested).
- **Ground truth travels with the data.**
  - Every fault records its root resource, onset, any triggering change, and the alarms it caused (`CAUSED` edges, `ground_truth.jsonl`).
  - RCA methods are scored against it without a human.
- **Real where we can, synthetic where we must, labelled.**
  - Only the backbone is real: Internet Topology Zoo PoPs and links (CC BY 4.0).
  - Everything else carries `synthetic: true`. On GÉANT 2012 the synthetic share is 99.5%.
- **Standard vocabularies, checked.** Labels and enumerations bind to samyama-ontologies builds:
  - NORIA-O classes and probable causes;
  - RFC 8632 severities;
  - TMF642 / X.733 alarm types.
  - A term the vocabularies lack stops the run. `binding.json` records the build files used, with sha256.

## Telecom pack

**Inventory.** A Topology Zoo backbone is expanded:
- per PoP: two core routers, aggregation routers dual-homed to both, and access and radio (RAN) sites homed to aggregation;
- devices from fictional vendors, then cards and ports, then cabling.
- Backbone links land on core-router ports.

**Services and customers.** Mobile data per RAN site, broadband per access site, and enterprise VPNs across PoPs with primary and backup paths.

**Faults.** Card, device, port, software crash, config change (always preceded by a failed `Change`), fibre cut, and site power loss.

**Network state follows from the topology, not from a script:**
- down ports;
- loss of signal at the far ends;
- devices with no path to the NOC become unreachable;
- services whose ends are unreachable go out.

**Alarms come from those states, on the resource that would report them:**
- the root alarm, when the root can still speak;
- loss of signal;
- unreachable device;
- service alarms;
- neighbour-cell link failures on live radio sites near a dead one;
- reroute congestion.

**Noise:**
- missing alarms;
- flapping;
- duplicates;
- clock skew per device;
- background alarms.

**Incidents.** The NOC's grouping is imperfect: alarms are mis-grouped, duplicate incidents appear, and background alarms get attached.
- History incidents carry their resolution and notes; they are the retrieval corpus.
- Eval incidents carry only the opening note, never the cause.

**Runbooks.** One per alarm type and resource kind.

**Splits.**
- `history`: past incidents, resolved.
- `eval`: held out for scoring. Scenario kinds are `single`, `storm` (one fault, many alarms) and `overlap` (two faults in one region within 10 minutes).

Output (schema in telecom-kg `schema/telecom_noc.cypher`):

| File | What |
|---|---|
| `nodes.jsonl`, `edges.jsonl` | The NOC layer on the real backbone, in the samyama-research-kit / samyama-ontologies shape |
| `alarms.stream.jsonl` | Raise and clear events in time order, for replay into a live graph |
| `ground_truth.jsonl` | Per fault: type, root resources, onset, repair, change, alarms caused, window |
| `binding.json` | The ontology terms used and the builds they came from |
| `manifest.json` | Seed, config, input and output sha256, counts, synthetic share |
| `realism.json` | From `sdg validate`: conformance, realism figures, baseline scores |

## Measured on `configs/telecom/small.yaml` (GÉANT 2012, seed 20261019)

```
nodes 11,701 · edges 14,883 · 530 devices · 7,308 ports · 218 services
70 faults (30 history, 40 eval) · 1,617 alarms · 79 incidents · conformance errors 0
alarms per fault: median 3, p90 41, max 111 · peak 73 alarms/minute · background 47%
faults whose root raised an alarm: 47 of 70
```

**Baselines on the 38 eval incidents** (a hit is the root device, or the PoP for a site power loss):

| Method | top-1 | top-3 |
|---|---:|---:|
| earliest critical alarm | 0.632 | 0.711 |
| most-alarmed device | 0.368 | 0.579 |
| random alarmed device | 0.263 | 0.579 |
| most recent change | 0.237 | 0.237 |

- **Earliest critical alarm is strong here,** because a card failure or software crash usually reports itself.
- It fails where the root is silent: device failure, power loss, and overlapping faults.
- That is where a graph method has to earn its place. telecom-kg's RCA benchmark scores the engine's methods against these baselines.

## Calibration: assumed, not measured

No openly licensed real telecom alarm log was found. TeleLogs is gated and asks not to be redistributed; the AIOps Challenge 2020 data is non-commercial and not telecom.

- Every rate, delay and probability in `configs/telecom/*.yaml` is an **assumption**:
  - alarm emission per mechanism;
  - loss-of-signal and polling delays;
  - flapping, duplicates and clock skew;
  - background rate;
  - repair time;
  - redundancy.
- They are in config so a partner's data can replace them.
- Storm sizes are bounded by the topology. A fibre cut on GÉANT's redundant backbone raises only a few alarms. The largest storms come from a site power loss or a failure that cuts off many radio sites.

## Licences

- **Code:** Apache-2.0 (to be confirmed with the repo owner).
- **Topology:** Internet Topology Zoo, CC BY 4.0.
- **Vocabulary terms in the output:**
  - NORIA-O, BSD-4-Clause, © Orange: "This product includes software developed by Orange." Review of the advertising clause is pending (samyama-ontologies#9).
  - RFC 8632 / 8345, Simplified BSD.
  - TMF642, Apache-2.0.

## Issues

#1 tracking · #2 core · #3 network · #4 faults and alarms · #5 incidents, changes, runbooks (LLM text pending) · #6 validation · #15 Rust GraphQL `@count` + Data Dictionary compiler & $O(1)$-memory streaming engine.

## Third-party vocabularies

The ontology build under `tests/fixtures/ontology-build/` is derived from third-party
vocabularies and is **not our work**:

| Directory | Source | Licence |
|---|---|---|
| `noria-o/` | NORIA-O 0.3, Orange | BSD-4-Clause — carries `LICENSE.txt` and `NOTICE.md`; the advertising clause applies, see `telecom-kg/LICENCE-REVIEW-NORIA-O.md` |
| `ietf-alarms/` | RFC 8632 (alarm management YANG) | Simplified BSD (IETF Trust) |
| `ietf-network-topology/` | RFC 8345 (network topology YANG) | Simplified BSD (IETF Trust) |
| `tmf642/` | TM Forum TMF642 Alarm Management API | Apache-2.0 |

A derived redistribution of NORIA-O must retain its copyright notice, conditions and
disclaimer. It did not until 2026-10-21; `tests/fixtures/ontology-build/noria-o/NOTICE.md`
records that.


## Citing this

This repository is the open-source compiler and generator artifact for:
1. *Schema-Driven Synthetic Data Generation as a Compiler Problem: Multi-Frontend Lowering,
   O(1)-Memory Relational/Graph Integrity, and Causal Ground Truth* (Samyama Research Team, 2026).
2. *Every Alarm Has a Cause: An Open Generator of Telecom Alarm Storms with Exact Root Causes,
   Bound to NORIA-O, RFC 8632 and TMF642* (Mandarapu and Kunkunuru, 2026).
