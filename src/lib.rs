//! Samyama Synthetic Data Compiler (`samyama-sdg`).
//!
//! Unifies:
//! 1. **Layer 1 — Multi-Frontend Declarative Structural & Attributional Compiler**:
//!    Compiles **GraphQL SDL (`.graphql`)**, **SQL DDL (`.sql`)**, or **OWL/RDFS Turtle (`.ttl`)**
//!    with `@count(N)`, `@pk`, `@regex`, `@constraint`, `@dist`, `@fk`, `@after`, `@derived`,
//!    `@edge`, `@weights`, and `@ontology` directives + Global Attribute Data Dictionary
//!    (`dictionary.rs`, `regex_gen.rs`) into `CompiledSchema` (`schema.rs`), executed via
//!    O(1)-memory index-bijective Foreign-Key synthesis and seekable counter-based parallel PRNGs
//!    (`rng.rs`, `engine.rs`, `validate_schema.rs`).
//! 2. **Layer 2 — Causal Process & Ground-Truth Simulator**:
//!    Ontology-bound (`ontology.rs`) Telecom NOC alarm-storm simulation (`telecom.rs`) with
//!    leak-free `ground_truth.jsonl` and `alarms.stream.jsonl`.

pub mod dictionary;
pub mod engine;
pub mod ontology;
pub mod regex_gen;
pub mod rng;
pub mod schema;
pub mod sql_frontend;
pub mod telecom;
pub mod ttl_frontend;
pub mod validate_schema;
