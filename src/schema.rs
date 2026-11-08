//! Multi-Frontend Schema Parser, Topological Sorter & Intermediate Representation (IR) Compiler.
//!
//! Compiles declarative data model specifications from any of three frontends:
//! 1. **GraphQL SDL (`.graphql` / `.gql`)** (`type Entity @count(N) @edge(...) { ... }`)
//! 2. **SQL DDL (`.sql` / `.ddl`)** (`CREATE TABLE ... PRIMARY KEY ... REFERENCES ...`)
//! 3. **OWL / RDFS / SKOS Turtle (`.ttl`)** (`owl:Class`, `owl:ObjectProperty`, `owl:DatatypeProperty`)
//!
//! Together with a `DataDictionary` and optional `samyama-ontologies` build directory into a
//! normalized, topologically sorted Intermediate Representation (`CompiledSchema`).

use crate::dictionary::{
    compile_rule, AttributeRuleSpec, CompiledValueGenerator, DataDictionary, DistSpec,
};
use crate::ontology::{bind, Binding, BindingSpec, EnumBindingSpec};
use crate::rng::{fnv1a64, PkKind};
use crate::sql_frontend::parse_sql_ddl;
use crate::ttl_frontend::parse_turtle_ontology;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaFrontendFormat {
    Graphql,
    SqlDdl,
    TurtleOntology,
}

impl SchemaFrontendFormat {
    pub fn from_path_or_content(path: Option<&Path>, content: &str) -> Self {
        if let Some(p) = path {
            if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                match ext.to_lowercase().as_str() {
                    "sql" | "ddl" => return Self::SqlDdl,
                    "ttl" | "turtle" | "n3" => return Self::TurtleOntology,
                    "graphql" | "gql" | "sdl" => return Self::Graphql,
                    _ => {}
                }
            }
        }
        Self::sniff_content(content)
    }

    pub fn sniff_content(content: &str) -> Self {
        for line in content.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') || t.starts_with("--") {
                continue;
            }
            let upper = t.to_uppercase();
            if upper.starts_with("CREATE TABLE ") || upper.starts_with("CREATE TYPE ") {
                return Self::SqlDdl;
            }
            if t.starts_with("@prefix")
                || t.starts_with("@base")
                || upper.starts_with("PREFIX ")
                || t.contains("owl:Class")
                || t.contains("rdfs:Class")
            {
                return Self::TurtleOntology;
            }
            break;
        }
        Self::Graphql
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FkDist {
    Uniform,
    Zipf { s: f64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemporalAfterConstraint {
    /// Either a local field on the same entity (e.g., `"startDate"`) or a parent field across a
    /// foreign-key reference (e.g., `"patient.dateOfBirth"`).
    pub reference_path: String,
    pub min_days: i64,
    pub max_days: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DerivedOp {
    Add,
    Sub,
    Mul,
    Div,
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
}

impl DerivedOp {
    pub fn is_comparison(self) -> bool {
        matches!(self, Self::Gt | Self::Ge | Self::Lt | Self::Le | Self::Eq)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DerivedOperand {
    Field(String),
    Constant(f64),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DerivedFormula {
    pub raw: String,
    pub lhs: DerivedOperand,
    pub op: DerivedOp,
    pub rhs: DerivedOperand,
    pub decimals: usize,
}

impl DerivedFormula {
    pub fn parse(formula: &str, is_int_or_bool: bool) -> Result<Self> {
        let trimmed = formula.trim();
        let ops: [(&str, DerivedOp); 9] = [
            (">=", DerivedOp::Ge),
            ("<=", DerivedOp::Le),
            ("==", DerivedOp::Eq),
            (">", DerivedOp::Gt),
            ("<", DerivedOp::Lt),
            ("+", DerivedOp::Add),
            ("-", DerivedOp::Sub),
            ("*", DerivedOp::Mul),
            ("/", DerivedOp::Div),
        ];
        for (sym, op) in ops {
            if let Some((l_str, r_str)) = trimmed.split_once(sym) {
                let l_trim = l_str.trim();
                let r_trim = r_str.trim();
                if !l_trim.is_empty() && !r_trim.is_empty() {
                    let lhs = if let Ok(v) = l_trim.parse::<f64>() {
                        DerivedOperand::Constant(v)
                    } else {
                        DerivedOperand::Field(l_trim.to_string())
                    };
                    let rhs = if let Ok(v) = r_trim.parse::<f64>() {
                        DerivedOperand::Constant(v)
                    } else {
                        DerivedOperand::Field(r_trim.to_string())
                    };
                    return Ok(Self {
                        raw: trimmed.to_string(),
                        lhs,
                        op,
                        rhs,
                        decimals: if is_int_or_bool { 0 } else { 2 },
                    });
                }
            }
        }
        bail!("unsupported @derived formula expression {:?}", formula)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompiledEdgeSpec {
    pub from_field: String,
    pub from_field_idx: usize,
    pub to_field: String,
    pub to_field_idx: usize,
    pub edge_label: String,
}

#[derive(Clone, Debug)]
pub enum CompiledFieldKind {
    PrimaryKey(PkKind),
    ForeignKey {
        target_entity: String,
        target_entity_idx: usize,
        edge_label: String,
        dist: FkDist,
    },
    /// Multi-valued foreign key list (e.g. `products: [Product!]!`) when not a reverse back-ref
    ForeignKeyList {
        target_entity: String,
        target_entity_idx: usize,
        edge_label: String,
        min_items: u32,
        max_items: u32,
        dist: FkDist,
    },
    Enum {
        enum_name: String,
        values: Vec<String>,
        cumulative_weights: Option<Vec<f64>>,
    },
    Value(CompiledValueGenerator),
    TemporalAfter {
        constraint: TemporalAfterConstraint,
        fallback_min_day: i64,
        fallback_max_day: i64,
    },
    Derived(DerivedFormula),
}

#[derive(Clone, Debug)]
pub struct CompiledField {
    pub name: String,
    pub name_hash: u64,
    pub raw_type: String,
    pub is_list: bool,
    pub non_null: bool,
    pub kind: CompiledFieldKind,
}

#[derive(Clone, Debug)]
pub struct CompiledEntity {
    pub name: String,
    pub name_hash: u64,
    pub count: u64,
    pub pk_kind: PkKind,
    pub pk_field_name: String,
    pub ontology_concepts: Vec<String>,
    pub edge_spec: Option<CompiledEdgeSpec>,
    pub fields: Vec<CompiledField>,
}

#[derive(Clone, Debug)]
pub struct CompiledSchema {
    pub frontend: SchemaFrontendFormat,
    pub entities: Vec<CompiledEntity>,
    /// Topological execution order (indices into `entities`, parents before children).
    pub topo_order: Vec<usize>,
    pub entity_index_by_name: HashMap<String, usize>,
    pub enums: BTreeMap<String, Vec<String>>,
    pub binding: Option<Binding>,
}

#[derive(Clone, Debug, Default)]
pub struct ParsedDirective {
    pub name: String,
    pub positional: Option<String>,
    pub args: BTreeMap<String, String>,
    pub list_args: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct RawField {
    pub name: String,
    pub base_type: String,
    pub is_list: bool,
    pub non_null: bool,
    pub directives: Vec<ParsedDirective>,
}

#[derive(Clone, Debug)]
pub struct RawEntity {
    pub name: String,
    pub count: u64,
    pub directives: Vec<ParsedDirective>,
    pub fields: Vec<RawField>,
}

#[derive(Clone, Debug)]
pub struct RawEnum {
    pub name: String,
    pub directives: Vec<ParsedDirective>,
    pub values: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RawScalar {
    pub name: String,
    pub directives: Vec<ParsedDirective>,
}

impl CompiledSchema {
    /// Compile a schema file (`.graphql`, `.sql`, or `.ttl`) by auto-detecting its frontend format.
    pub fn compile_file(
        schema_path: &Path,
        dict: DataDictionary,
        ontologies_build: Option<&Path>,
        scale_factor: f64,
        count_overrides: &BTreeMap<String, u64>,
    ) -> Result<Self> {
        let content = std::fs::read_to_string(schema_path)?;
        let fmt = SchemaFrontendFormat::from_path_or_content(Some(schema_path), &content);
        Self::compile_with_format(
            &content,
            fmt,
            dict,
            ontologies_build,
            scale_factor,
            count_overrides,
        )
    }

    /// Compile a schema string (auto-detecting GraphQL SDL, SQL DDL, or Turtle Ontology).
    pub fn compile(
        schema_text: &str,
        dict: DataDictionary,
        ontologies_build: Option<&Path>,
        scale_factor: f64,
        count_overrides: &BTreeMap<String, u64>,
    ) -> Result<Self> {
        let fmt = SchemaFrontendFormat::from_path_or_content(None, schema_text);
        Self::compile_with_format(
            schema_text,
            fmt,
            dict,
            ontologies_build,
            scale_factor,
            count_overrides,
        )
    }

    /// Compile a schema string using an explicit frontend format.
    pub fn compile_with_format(
        schema_text: &str,
        frontend: SchemaFrontendFormat,
        mut dict: DataDictionary,
        ontologies_build: Option<&Path>,
        scale_factor: f64,
        count_overrides: &BTreeMap<String, u64>,
    ) -> Result<Self> {
        let (raw_entities, raw_enums, raw_scalars) = match frontend {
            SchemaFrontendFormat::Graphql => parse_graphql_sdl(schema_text)?,
            SchemaFrontendFormat::SqlDdl => parse_sql_ddl(schema_text)?,
            SchemaFrontendFormat::TurtleOntology => parse_turtle_ontology(schema_text)?,
        };
        if raw_entities.is_empty() {
            bail!(
                "schema ({:?}) contains no entity/table/class definitions",
                frontend
            );
        }

        // 1. Register custom scalar directives into DataDictionary
        for sc in &raw_scalars {
            if let Some(rule) = directives_to_rule(&sc.directives) {
                if let Some(gen) = compile_rule(&rule)? {
                    dict.scalars.insert(sc.name.clone(), gen);
                }
            }
        }

        // 2. Resolve ontology bindings if any entity or enum carries `@ontology`
        let mut binding_spec = BindingSpec::default();
        let mut vocab_set = BTreeSet::new();
        for ent in &raw_entities {
            if let Some(d) = ent.directives.iter().find(|d| d.name == "ontology") {
                if let Some(v) = d.args.get("vocabulary") {
                    vocab_set.insert(v.clone());
                }
                let concepts = if let Some(list) = d.list_args.get("concepts") {
                    list.clone()
                } else if let Some(c) = d.args.get("concept").or(d.positional.as_ref()) {
                    vec![c.clone()]
                } else {
                    Vec::new()
                };
                for c in &concepts {
                    if let Some((v, _)) = c.split_once(':') {
                        let mapped = match v {
                            "noria" => "noria-o",
                            "ietf-network" => "ietf-network-topology",
                            other => other,
                        };
                        vocab_set.insert(mapped.to_string());
                    }
                }
                binding_spec.labels.insert(ent.name.clone(), concepts);
            }
        }

        for en in &raw_enums {
            if let Some(d) = en.directives.iter().find(|d| d.name == "ontology") {
                let vocab = d
                    .args
                    .get("vocabulary")
                    .ok_or_else(|| anyhow!("enum {} @ontology requires vocabulary", en.name))?
                    .clone();
                vocab_set.insert(vocab.clone());
                binding_spec.enums.insert(
                    en.name.clone(),
                    EnumBindingSpec {
                        vocabulary: vocab,
                        owner: d.args.get("owner").cloned(),
                        scheme: d.args.get("scheme").cloned(),
                        exclude: d.list_args.get("exclude").cloned().unwrap_or_default(),
                    },
                );
            }
        }

        let resolved_binding = if !vocab_set.is_empty() {
            binding_spec.vocabularies = vocab_set.into_iter().collect();
            for ent in &raw_entities {
                binding_spec.labels.entry(ent.name.clone()).or_default();
            }
            let build_dir = ontologies_build.ok_or_else(|| {
                anyhow!("schema uses @ontology directives but no --ontologies build path was provided")
            })?;
            Some(bind(build_dir, &binding_spec)?)
        } else {
            None
        };

        // 3. Build enum value map + optional enum-level `@weights`
        let mut enums: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut enum_weights: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for en in &raw_enums {
            if let Some(d) = en.directives.iter().find(|d| d.name == "weights") {
                if let Some(w_list) = d.list_args.get("values").or_else(|| d.list_args.get("items"))
                {
                    if let Some(cum) = parse_cumulative_weights(w_list) {
                        enum_weights.insert(en.name.clone(), cum);
                    }
                }
            }
            if let Some(b) = &resolved_binding {
                let bound_vals = b.values(&en.name);
                if !bound_vals.is_empty() {
                    enums.insert(en.name.clone(), bound_vals);
                    continue;
                }
            }
            if en.values.is_empty() {
                bail!("enum {:?} has no values", en.name);
            }
            enums.insert(en.name.clone(), en.values.clone());
        }

        // 4. Index entity names (both exact and case-insensitive fallback for SQL foreign keys)
        let mut entity_index_by_name: HashMap<String, usize> = HashMap::new();
        let mut entity_index_lower: HashMap<String, usize> = HashMap::new();
        for (idx, ent) in raw_entities.iter().enumerate() {
            if entity_index_by_name.insert(ent.name.clone(), idx).is_some() {
                bail!("duplicate entity type definition {:?}", ent.name);
            }
            entity_index_lower.insert(ent.name.to_lowercase(), idx);
        }

        let resolve_entity_idx = |type_name: &str| -> Option<usize> {
            entity_index_by_name
                .get(type_name)
                .copied()
                .or_else(|| entity_index_lower.get(&type_name.to_lowercase()).copied())
        };

        // Detect which scalar FK edges exist `(Child -> Parent)` so reverse `[Child!]!` lists on
        // `Parent` are recognized as virtual back-references rather than circular dependencies.
        let mut scalar_fk_pairs: BTreeSet<(String, String)> = BTreeSet::new();
        for ent in &raw_entities {
            for f in &ent.fields {
                if !f.is_list {
                    if let Some(target_idx) = resolve_entity_idx(&f.base_type) {
                        scalar_fk_pairs
                            .insert((ent.name.clone(), raw_entities[target_idx].name.clone()));
                    }
                }
            }
        }

        // 5. Compile entities and fields
        let mut compiled_entities = Vec::with_capacity(raw_entities.len());
        for ent in &raw_entities {
            let base_count = if let Some(&ov) = count_overrides.get(&ent.name) {
                ov
            } else {
                ((ent.count as f64) * scale_factor).round().max(1.0) as u64
            };
            let default_prefix = ent
                .name
                .chars()
                .take(3)
                .collect::<String>()
                .to_lowercase();
            let mut entity_pk_kind = ent
                .directives
                .iter()
                .find(|d| d.name == "pk")
                .and_then(|d| d.args.get("kind").or(d.positional.as_ref()))
                .map(|s| PkKind::parse(s, &default_prefix))
                .unwrap_or(PkKind::Uuid);

            let ontology_concepts = binding_spec
                .labels
                .get(&ent.name)
                .cloned()
                .unwrap_or_default();

            let mut pk_field_name = "id".to_string();
            let mut compiled_fields = Vec::new();

            for f in &ent.fields {
                let has_explicit_fk_dir = f
                    .directives
                    .iter()
                    .any(|d| d.name == "fk" || d.name == "cardinality");
                if f.is_list {
                    if let Some(t_idx) = resolve_entity_idx(&f.base_type) {
                        let target_canonical = &raw_entities[t_idx].name;
                        if scalar_fk_pairs.contains(&(target_canonical.clone(), ent.name.clone()))
                            && !has_explicit_fk_dir
                        {
                            continue;
                        }
                    }
                }

                let is_pk = f.base_type == "ID"
                    || f.name == "id"
                    || f.directives.iter().any(|d| d.name == "pk");
                let kind = if is_pk {
                    if let Some(d) = f.directives.iter().find(|d| d.name == "pk") {
                        if let Some(k) = d.args.get("kind").or(d.positional.as_ref()) {
                            entity_pk_kind = PkKind::parse(k, &default_prefix);
                        }
                    }
                    pk_field_name = f.name.clone();
                    CompiledFieldKind::PrimaryKey(entity_pk_kind.clone())
                } else if let Some(derived_dir) =
                    f.directives.iter().find(|d| d.name == "derived")
                {
                    let formula_str = derived_dir
                        .args
                        .get("formula")
                        .or(derived_dir.positional.as_ref())
                        .ok_or_else(|| {
                            anyhow!(
                                "{}.{} @derived directive requires `formula` argument",
                                ent.name,
                                f.name
                            )
                        })?;
                    let is_int_or_bool = matches!(f.base_type.as_str(), "Int" | "Boolean");
                    CompiledFieldKind::Derived(DerivedFormula::parse(formula_str, is_int_or_bool)?)
                } else if let Some(target_idx) = resolve_entity_idx(&f.base_type) {
                    let target_canonical = raw_entities[target_idx].name.clone();
                    let fk_dir = f.directives.iter().find(|d| d.name == "fk");
                    let dist = match fk_dir.and_then(|d| d.args.get("dist").map(|s| s.as_str())) {
                        Some("zipf") => {
                            let s = fk_dir
                                .and_then(|d| d.args.get("s"))
                                .and_then(|v| v.parse::<f64>().ok())
                                .unwrap_or(1.2);
                            FkDist::Zipf { s }
                        }
                        _ => FkDist::Uniform,
                    };
                    let default_edge = f.name.to_uppercase();
                    let edge_label = fk_dir
                        .and_then(|d| d.args.get("edge"))
                        .cloned()
                        .unwrap_or(default_edge);

                    if f.is_list {
                        let card_dir = f.directives.iter().find(|d| d.name == "cardinality");
                        let min_items = card_dir
                            .and_then(|d| d.args.get("min"))
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1);
                        let max_items = card_dir
                            .and_then(|d| d.args.get("max"))
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(4);
                        CompiledFieldKind::ForeignKeyList {
                            target_entity: target_canonical,
                            target_entity_idx: target_idx,
                            edge_label,
                            min_items,
                            max_items,
                            dist,
                        }
                    } else {
                        CompiledFieldKind::ForeignKey {
                            target_entity: target_canonical,
                            target_entity_idx: target_idx,
                            edge_label,
                            dist,
                        }
                    }
                } else if let Some(vals) = enums.get(&f.base_type) {
                    let field_weights = f
                        .directives
                        .iter()
                        .find(|d| d.name == "weights")
                        .and_then(|d| {
                            d.list_args
                                .get("values")
                                .or_else(|| d.list_args.get("items"))
                        })
                        .and_then(|w| parse_cumulative_weights(w))
                        .or_else(|| enum_weights.get(&f.base_type).cloned());
                    CompiledFieldKind::Enum {
                        enum_name: f.base_type.clone(),
                        values: vals.clone(),
                        cumulative_weights: field_weights,
                    }
                } else if let Some(after_dir) = f.directives.iter().find(|d| d.name == "after") {
                    let reference_path = after_dir
                        .args
                        .get("field")
                        .or(after_dir.positional.as_ref())
                        .ok_or_else(|| {
                            anyhow!(
                                "{}.{} @after directive requires `field` argument",
                                ent.name,
                                f.name
                            )
                        })?
                        .clone();
                    let min_days = after_dir
                        .args
                        .get("min_days")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(1);
                    let max_days = after_dir
                        .args
                        .get("max_days")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(365);
                    CompiledFieldKind::TemporalAfter {
                        constraint: TemporalAfterConstraint {
                            reference_path,
                            min_days,
                            max_days,
                        },
                        fallback_min_day: 18_262,
                        fallback_max_day: 20_758,
                    }
                } else if let Some(rule) = directives_to_rule(&f.directives) {
                    if let Some(gen) = compile_rule(&rule)? {
                        CompiledFieldKind::Value(gen)
                    } else {
                        CompiledFieldKind::Value(dict.resolve(&ent.name, &f.name, &f.base_type)?)
                    }
                } else {
                    CompiledFieldKind::Value(dict.resolve(&ent.name, &f.name, &f.base_type)?)
                };

                compiled_fields.push(CompiledField {
                    name: f.name.clone(),
                    name_hash: fnv1a64(f.name.as_bytes()),
                    raw_type: f.base_type.clone(),
                    is_list: f.is_list,
                    non_null: f.non_null,
                    kind,
                });
            }

            // Ensure every entity has a primary key field (prepend `id` if omitted)
            if !compiled_fields
                .iter()
                .any(|f| matches!(f.kind, CompiledFieldKind::PrimaryKey(_)))
            {
                compiled_fields.insert(
                    0,
                    CompiledField {
                        name: "id".to_string(),
                        name_hash: fnv1a64(b"id"),
                        raw_type: "ID".to_string(),
                        is_list: false,
                        non_null: true,
                        kind: CompiledFieldKind::PrimaryKey(entity_pk_kind.clone()),
                    },
                );
            }

            // Check if entity carries first-class `@edge(from: "...", to: "...", type: "...")`
            let edge_spec = if let Some(ed) = ent.directives.iter().find(|d| d.name == "edge") {
                let from_field = ed
                    .args
                    .get("from")
                    .or_else(|| ed.args.get("src"))
                    .ok_or_else(|| anyhow!("type {} @edge requires `from` argument", ent.name))?
                    .clone();
                let to_field = ed
                    .args
                    .get("to")
                    .or_else(|| ed.args.get("dst"))
                    .ok_or_else(|| anyhow!("type {} @edge requires `to` argument", ent.name))?
                    .clone();
                let from_field_idx = compiled_fields
                    .iter()
                    .position(|f| f.name == from_field)
                    .ok_or_else(|| {
                        anyhow!(
                            "type {} @edge `from` field {:?} not found",
                            ent.name,
                            from_field
                        )
                    })?;
                let to_field_idx = compiled_fields
                    .iter()
                    .position(|f| f.name == to_field)
                    .ok_or_else(|| {
                        anyhow!(
                            "type {} @edge `to` field {:?} not found",
                            ent.name,
                            to_field
                        )
                    })?;
                let edge_label = ed
                    .args
                    .get("type")
                    .or_else(|| ed.args.get("label"))
                    .or(ed.positional.as_ref())
                    .cloned()
                    .unwrap_or_else(|| ent.name.to_uppercase());
                Some(CompiledEdgeSpec {
                    from_field,
                    from_field_idx,
                    to_field,
                    to_field_idx,
                    edge_label,
                })
            } else {
                None
            };

            compiled_entities.push(CompiledEntity {
                name: ent.name.clone(),
                name_hash: fnv1a64(ent.name.as_bytes()),
                count: base_count,
                pk_kind: entity_pk_kind,
                pk_field_name,
                ontology_concepts,
                edge_spec,
                fields: compiled_fields,
            });
        }

        // 6. Topological sort (Kahn's algorithm): parent entities before dependent child entities
        let n = compiled_entities.len();
        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (child_idx, ent) in compiled_entities.iter().enumerate() {
            let mut deps = BTreeSet::new();
            for f in &ent.fields {
                match &f.kind {
                    CompiledFieldKind::ForeignKey {
                        target_entity_idx, ..
                    }
                    | CompiledFieldKind::ForeignKeyList {
                        target_entity_idx, ..
                    } => {
                        if *target_entity_idx != child_idx {
                            deps.insert(*target_entity_idx);
                        }
                    }
                    _ => {}
                }
            }
            for parent_idx in deps {
                adj[parent_idx].push(child_idx);
                in_degree[child_idx] += 1;
            }
        }

        let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
        let mut topo_order = Vec::with_capacity(n);
        while let Some(u) = queue.pop_front() {
            topo_order.push(u);
            for &v in &adj[u] {
                in_degree[v] -= 1;
                if in_degree[v] == 0 {
                    queue.push_back(v);
                }
            }
        }
        if topo_order.len() != n {
            bail!("cyclic foreign-key dependency detected among entities in schema");
        }

        Ok(Self {
            frontend,
            entities: compiled_entities,
            topo_order,
            entity_index_by_name,
            enums,
            binding: resolved_binding,
        })
    }
}

fn parse_cumulative_weights(items: &[String]) -> Option<Vec<f64>> {
    let mut weights = Vec::with_capacity(items.len());
    let mut sum = 0.0;
    for s in items {
        let w = s.parse::<f64>().ok()?;
        if w > 0.0 {
            sum += w;
            weights.push(sum);
        }
    }
    if sum <= 0.0 || weights.is_empty() {
        return None;
    }
    for w in &mut weights {
        *w /= sum;
    }
    if let Some(last) = weights.last_mut() {
        *last = 1.0;
    }
    Some(weights)
}

fn directives_to_rule(directives: &[ParsedDirective]) -> Option<AttributeRuleSpec> {
    let mut rule = AttributeRuleSpec {
        regex: None,
        values: None,
        dist: None,
        date_range: None,
    };
    let mut matched = false;
    for d in directives {
        match d.name.as_str() {
            "regex" => {
                if let Some(p) = d.args.get("pattern").or(d.positional.as_ref()) {
                    rule.regex = Some(p.clone());
                    matched = true;
                }
            }
            "constraint" => {
                // Compatibility with Samyama.ai/data_generator `@constraint(pattern: "...", min: ..., max: ...)`
                if let Some(p) = d.args.get("pattern").or(d.positional.as_ref()) {
                    rule.regex = Some(p.clone());
                    matched = true;
                }
                let min = d.args.get("min").and_then(|s| s.parse().ok());
                let max = d.args.get("max").and_then(|s| s.parse().ok());
                if min.is_some() || max.is_some() {
                    rule.dist = Some(DistSpec {
                        kind: d
                            .args
                            .get("kind")
                            .cloned()
                            .unwrap_or_else(|| "uniform".to_string()),
                        min,
                        max,
                        mean: d.args.get("mean").and_then(|s| s.parse().ok()),
                        std: d.args.get("std").and_then(|s| s.parse().ok()),
                        s: d.args.get("s").and_then(|s| s.parse().ok()),
                        decimals: d.args.get("decimals").and_then(|s| s.parse().ok()),
                    });
                    matched = true;
                }
            }
            "values" => {
                if let Some(items) = d.list_args.get("items").or_else(|| d.list_args.get("values"))
                {
                    rule.values = Some(items.clone());
                    matched = true;
                }
            }
            "dist" => {
                let kind = d
                    .args
                    .get("kind")
                    .or(d.positional.as_ref())
                    .cloned()
                    .unwrap_or_else(|| "uniform".to_string());
                let min = d.args.get("min").and_then(|s| s.parse().ok());
                let max = d.args.get("max").and_then(|s| s.parse().ok());
                let mean = d.args.get("mean").and_then(|s| s.parse().ok());
                let std = d.args.get("std").and_then(|s| s.parse().ok());
                let s = d.args.get("s").and_then(|s| s.parse().ok());
                let decimals = d.args.get("decimals").and_then(|s| s.parse().ok());
                rule.dist = Some(DistSpec {
                    kind,
                    min,
                    max,
                    mean,
                    std,
                    s,
                    decimals,
                });
                matched = true;
            }
            _ => {}
        }
    }
    if matched {
        Some(rule)
    } else {
        None
    }
}

pub(crate) fn parse_directives_from_str(s: &str) -> Result<Vec<ParsedDirective>> {
    let chars: Vec<char> = s.chars().collect();
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < chars.len() {
        if chars[pos] == '@' {
            let dirs = parse_directives(&chars, &mut pos)?;
            out.extend(dirs);
        } else {
            pos += 1;
        }
    }
    Ok(out)
}

fn strip_comments(sdl: &str) -> String {
    sdl.lines()
        .map(|line| {
            let mut in_quotes = false;
            let mut prev_escape = false;
            for (idx, ch) in line.char_indices() {
                if ch == '"' && !prev_escape {
                    in_quotes = !in_quotes;
                } else if ch == '#' && !in_quotes {
                    return &line[..idx];
                }
                prev_escape = ch == '\\' && !prev_escape;
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_graphql_sdl(sdl: &str) -> Result<(Vec<RawEntity>, Vec<RawEnum>, Vec<RawScalar>)> {
    let cleaned = strip_comments(sdl);
    let mut entities = Vec::new();
    let mut enums = Vec::new();
    let mut scalars = Vec::new();

    let chars: Vec<char> = cleaned.chars().collect();
    let mut pos = 0;

    while pos < chars.len() {
        skip_ws(&chars, &mut pos);
        if pos >= chars.len() {
            break;
        }
        let kw = read_ident(&chars, &mut pos)?;
        match kw.as_str() {
            "type" => {
                skip_ws(&chars, &mut pos);
                let name = read_ident(&chars, &mut pos)?;
                let directives = parse_directives(&chars, &mut pos)?;
                skip_ws(&chars, &mut pos);
                if pos >= chars.len() || chars[pos] != '{' {
                    bail!("expected '{{' after type {}", name);
                }
                pos += 1; // skip '{'
                let count = directives
                    .iter()
                    .find(|d| d.name == "count")
                    .and_then(|d| {
                        d.positional
                            .as_ref()
                            .or_else(|| d.args.get("n"))
                            .or_else(|| d.args.get("value"))
                    })
                    .and_then(|s| s.replace('_', "").parse::<u64>().ok())
                    .unwrap_or(10);

                let mut fields = Vec::new();
                loop {
                    skip_ws(&chars, &mut pos);
                    if pos >= chars.len() {
                        bail!("unclosed '{{' in type {}", name);
                    }
                    if chars[pos] == '}' {
                        pos += 1;
                        break;
                    }
                    let field_name = read_ident(&chars, &mut pos)?;
                    skip_ws(&chars, &mut pos);
                    if pos >= chars.len() || chars[pos] != ':' {
                        bail!("expected ':' after field {}.{}", name, field_name);
                    }
                    pos += 1; // skip ':'
                    skip_ws(&chars, &mut pos);
                    let (base_type, is_list, non_null) = parse_type_ref(&chars, &mut pos)?;
                    let field_dirs = parse_directives(&chars, &mut pos)?;
                    fields.push(RawField {
                        name: field_name,
                        base_type,
                        is_list,
                        non_null,
                        directives: field_dirs,
                    });
                }
                entities.push(RawEntity {
                    name,
                    count,
                    directives,
                    fields,
                });
            }
            "enum" => {
                skip_ws(&chars, &mut pos);
                let name = read_ident(&chars, &mut pos)?;
                let directives = parse_directives(&chars, &mut pos)?;
                skip_ws(&chars, &mut pos);
                if pos >= chars.len() || chars[pos] != '{' {
                    bail!("expected '{{' after enum {}", name);
                }
                pos += 1;
                let mut values = Vec::new();
                loop {
                    skip_ws(&chars, &mut pos);
                    if pos >= chars.len() {
                        bail!("unclosed '{{' in enum {}", name);
                    }
                    if chars[pos] == '}' {
                        pos += 1;
                        break;
                    }
                    let val = read_ident(&chars, &mut pos)?;
                    let _ = parse_directives(&chars, &mut pos)?;
                    values.push(val);
                }
                enums.push(RawEnum {
                    name,
                    directives,
                    values,
                });
            }
            "scalar" => {
                skip_ws(&chars, &mut pos);
                let name = read_ident(&chars, &mut pos)?;
                let directives = parse_directives(&chars, &mut pos)?;
                scalars.push(RawScalar { name, directives });
            }
            other => bail!("unexpected top-level GraphQL keyword {:?}", other),
        }
    }

    Ok((entities, enums, scalars))
}

fn skip_ws(chars: &[char], pos: &mut usize) {
    while *pos < chars.len() && (chars[*pos].is_whitespace() || chars[*pos] == ',') {
        *pos += 1;
    }
}

fn read_ident(chars: &[char], pos: &mut usize) -> Result<String> {
    skip_ws(chars, pos);
    let start = *pos;
    while *pos < chars.len()
        && (chars[*pos].is_alphanumeric() || chars[*pos] == '_' || chars[*pos] == '-')
    {
        *pos += 1;
    }
    if *pos == start {
        bail!(
            "expected identifier at position {} (found {:?})",
            *pos,
            chars.get(*pos)
        );
    }
    Ok(chars[start..*pos].iter().collect())
}

fn parse_type_ref(chars: &[char], pos: &mut usize) -> Result<(String, bool, bool)> {
    skip_ws(chars, pos);
    let mut is_list = false;
    if *pos < chars.len() && chars[*pos] == '[' {
        is_list = true;
        *pos += 1;
    }
    let base = read_ident(chars, pos)?;
    let mut non_null = false;
    if *pos < chars.len() && chars[*pos] == '!' {
        non_null = true;
        *pos += 1;
    }
    if is_list {
        skip_ws(chars, pos);
        if *pos >= chars.len() || chars[*pos] != ']' {
            bail!("expected ']' in list type reference for {}", base);
        }
        *pos += 1;
        if *pos < chars.len() && chars[*pos] == '!' {
            non_null = true;
            *pos += 1;
        }
    }
    Ok((base, is_list, non_null))
}

fn parse_directives(chars: &[char], pos: &mut usize) -> Result<Vec<ParsedDirective>> {
    let mut out = Vec::new();
    loop {
        let saved = *pos;
        skip_ws(chars, pos);
        if *pos >= chars.len() || chars[*pos] != '@' {
            *pos = saved;
            break;
        }
        *pos += 1; // skip '@'
        let name = read_ident(chars, pos)?;
        let mut dir = ParsedDirective {
            name,
            positional: None,
            args: BTreeMap::new(),
            list_args: BTreeMap::new(),
        };
        if *pos < chars.len() && chars[*pos] == '(' {
            *pos += 1; // skip '('
            loop {
                skip_ws(chars, pos);
                if *pos >= chars.len() {
                    bail!("unclosed '(' in directive @{}", dir.name);
                }
                if chars[*pos] == ')' {
                    *pos += 1;
                    break;
                }
                let token = read_value_or_ident(chars, pos)?;
                skip_ws(chars, pos);
                if *pos < chars.len() && chars[*pos] == ':' {
                    *pos += 1; // skip ':'
                    skip_ws(chars, pos);
                    if *pos < chars.len() && chars[*pos] == '[' {
                        let list = parse_string_list(chars, pos)?;
                        dir.list_args.insert(token, list);
                    } else {
                        let val = read_value_or_ident(chars, pos)?;
                        dir.args.insert(token, val);
                    }
                } else {
                    dir.positional = Some(token);
                }
            }
        }
        out.push(dir);
    }
    Ok(out)
}

fn parse_string_list(chars: &[char], pos: &mut usize) -> Result<Vec<String>> {
    *pos += 1; // skip '['
    let mut items = Vec::new();
    loop {
        skip_ws(chars, pos);
        if *pos >= chars.len() {
            bail!("unclosed '[' in directive list argument");
        }
        if chars[*pos] == ']' {
            *pos += 1;
            break;
        }
        items.push(read_value_or_ident(chars, pos)?);
    }
    Ok(items)
}

fn read_value_or_ident(chars: &[char], pos: &mut usize) -> Result<String> {
    skip_ws(chars, pos);
    if *pos >= chars.len() {
        bail!("unexpected end of directive argument");
    }
    if chars[*pos] == '"' {
        *pos += 1;
        let mut s = String::new();
        while *pos < chars.len() && chars[*pos] != '"' {
            if chars[*pos] == '\\' && *pos + 1 < chars.len() {
                *pos += 1;
                s.push(chars[*pos]);
            } else {
                s.push(chars[*pos]);
            }
            *pos += 1;
        }
        if *pos >= chars.len() {
            bail!("unclosed string literal in directive");
        }
        *pos += 1; // skip '"'
        Ok(s)
    } else {
        let start = *pos;
        while *pos < chars.len()
            && !chars[*pos].is_whitespace()
            && !matches!(chars[*pos], ',' | ')' | ':' | ']')
        {
            *pos += 1;
        }
        Ok(chars[start..*pos].iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_2023_health_clinic_graphql_with_counts_and_backrefs() {
        let sdl = r#"
            type Appointment @count(20) {
                id: ID!
                patient: Patient!
                doctor: Doctor!
                appointmentDate: Date! @after(field: "patient.dateOfBirth", min_days: 7300, max_days: 25000)
                appointmentTime: Time!
                status: AppointmentStatus!
            }

            type Patient @count(10) {
                id: ID!
                firstName: String!
                lastName: String!
                email: Email!
                dateOfBirth: Date!
                appointments: [Appointment!]!
            }

            type Doctor @count(5) {
                id: ID!
                firstName: String!
                lastName: String!
                email: Email!
                specialization: Specialization!
                appointments: [Appointment!]!
            }

            enum Specialization {
                CARDIOLOGY
                DERMATOLOGY
                NEUROLOGY
                ONCOLOGY
                PEDIATRICS
            }

            enum AppointmentStatus {
                SCHEDULED
                COMPLETED
                CANCELED
            }

            scalar Email
            scalar Date
            scalar Time
        "#;

        let dict = DataDictionary::built_in().unwrap();
        let compiled =
            CompiledSchema::compile(sdl, dict, None, 1.0, &BTreeMap::new()).expect("compiles");
        assert_eq!(compiled.entities.len(), 3);
        let ordered_names: Vec<&str> = compiled
            .topo_order
            .iter()
            .map(|&i| compiled.entities[i].name.as_str())
            .collect();
        let appt_pos = ordered_names
            .iter()
            .position(|&n| n == "Appointment")
            .unwrap();
        let pat_pos = ordered_names.iter().position(|&n| n == "Patient").unwrap();
        let doc_pos = ordered_names.iter().position(|&n| n == "Doctor").unwrap();
        assert!(pat_pos < appt_pos, "Patient must precede Appointment");
        assert!(doc_pos < appt_pos, "Doctor must precede Appointment");
    }
}
