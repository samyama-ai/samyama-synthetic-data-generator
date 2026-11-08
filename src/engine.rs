//! High-Throughput, O(1)-Memory Parallel Execution Engine for `CompiledSchema`.
//!
//! Executes a compiled schema IR (`CompiledSchema` from GraphQL SDL, SQL DDL, or OWL/Turtle) using:
//! - Counter-based seekable PRNGs (`SeekableRng::for_field_hashes`) per `(entity, row_idx, field)`,
//! - O(1)-memory index-bijective Foreign-Key synthesis (`PK(target_entity, parent_idx, seed)`),
//! - O(1)-memory recursive cross-entity temporal resolution (`@after(field: "patient.dateOfBirth")`),
//! - O(1)-memory intra-row `@derived(formula: "...")` evaluation,
//! - First-class attributed property-graph edges (`@edge(from: "...", to: "...", type: "...")`),
//! - Data-parallel Rayon chunk formatting with deterministic ordered streaming and on-the-fly SHA-256.

use crate::dictionary::{
    format_date_from_epoch_days, CompiledValueGenerator, DistSpec,
};
use crate::ontology::sha256_file;
use crate::rng::SeekableRng;
use crate::schema::{
    CompiledEntity, CompiledField, CompiledFieldKind, CompiledSchema, DerivedFormula, DerivedOp,
    DerivedOperand, FkDist,
};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

const CHUNK_ROWS: u64 = 8_192;
const WINDOW_CHUNKS: u64 = 16; // 131,072 rows per parallel window -> bounded ~15 MB RAM

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputFormat {
    Csv,
    Graph,
    Both,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "csv" => Ok(Self::Csv),
            "graph" | "jsonl" => Ok(Self::Graph),
            "both" | "all" => Ok(Self::Both),
            other => Err(anyhow!(
                "unknown output format {:?}; expected csv, graph, or both",
                other
            )),
        }
    }
}

struct HashingWriter {
    writer: BufWriter<File>,
    hasher: Sha256,
}

impl HashingWriter {
    fn create(path: &Path) -> Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::with_capacity(1 << 20, file),
            hasher: Sha256::new(),
        })
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        self.writer.write_all(buf)?;
        self.hasher.update(buf);
        Ok(())
    }

    fn finish(mut self) -> Result<String> {
        self.writer.flush()?;
        Ok(format!("{:x}", self.hasher.finalize()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchemaManifest {
    pub generator: String,
    pub mode: String,
    pub seed: u64,
    pub scale_factor: f64,
    pub format: String,
    pub schema_file: Option<String>,
    pub dictionary_file: Option<String>,
    pub inputs: BTreeMap<String, String>,
    pub outputs: BTreeMap<String, String>,
    pub counts: SchemaCounts,
    pub total_records: u64,
    pub total_edges: u64,
    pub duration_ms: f64,
    pub records_per_sec: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SchemaCounts {
    pub entities: BTreeMap<String, u64>,
    pub relations: BTreeMap<String, u64>,
}

/// Evaluate which parent index `p in [0, target.count)` a foreign-key field points to.
#[inline]
pub fn eval_fk_target_index(
    schema: &CompiledSchema,
    seed: u64,
    entity: &CompiledEntity,
    row_idx: u64,
    field: &CompiledField,
    target_entity_idx: usize,
    dist: &FkDist,
) -> u64 {
    let target_count = schema.entities[target_entity_idx].count.max(1);
    let mut rng = SeekableRng::for_field_hashes(seed, entity.name_hash, row_idx, field.name_hash);
    match dist {
        FkDist::Uniform => rng.integers(target_count),
        FkDist::Zipf { s } => rng.zipf_index(target_count, *s),
    }
}

/// Evaluate a date field in epoch days (supporting recursive cross-entity `@after` resolution in O(1) memory).
pub fn eval_field_epoch_days(
    schema: &CompiledSchema,
    seed: u64,
    entity_idx: usize,
    row_idx: u64,
    field_name: &str,
    depth: u32,
) -> i64 {
    if depth > 8 {
        return 18_262;
    }
    let ent = &schema.entities[entity_idx];
    let Some(field) = ent.fields.iter().find(|f| f.name == field_name) else {
        return 18_262;
    };
    let mut rng = SeekableRng::for_field_hashes(seed, ent.name_hash, row_idx, field.name_hash);
    match &field.kind {
        CompiledFieldKind::Value(CompiledValueGenerator::DateDays { min_day, max_day }) => {
            rng.range_i64(*min_day, *max_day)
        }
        CompiledFieldKind::TemporalAfter {
            constraint,
            fallback_min_day,
            fallback_max_day,
        } => {
            let base_days = if let Some((fk_field_name, parent_field_name)) =
                constraint.reference_path.split_once('.')
            {
                if let Some(fk_field) = ent.fields.iter().find(|f| f.name == fk_field_name) {
                    if let CompiledFieldKind::ForeignKey {
                        target_entity_idx,
                        dist,
                        ..
                    } = &fk_field.kind
                    {
                        let parent_row_idx = eval_fk_target_index(
                            schema,
                            seed,
                            ent,
                            row_idx,
                            fk_field,
                            *target_entity_idx,
                            dist,
                        );
                        eval_field_epoch_days(
                            schema,
                            seed,
                            *target_entity_idx,
                            parent_row_idx,
                            parent_field_name,
                            depth + 1,
                        )
                    } else {
                        rng.range_i64(*fallback_min_day, *fallback_max_day)
                    }
                } else {
                    rng.range_i64(*fallback_min_day, *fallback_max_day)
                }
            } else {
                eval_field_epoch_days(
                    schema,
                    seed,
                    entity_idx,
                    row_idx,
                    &constraint.reference_path,
                    depth + 1,
                )
            };
            base_days + rng.range_i64(constraint.min_days, constraint.max_days)
        }
        _ => 18_262,
    }
}

#[inline]
fn sample_dist_i64(rng: &mut SeekableRng, d: &DistSpec) -> i64 {
    let lo = d.min.unwrap_or(0.0);
    let hi = d.max.unwrap_or(100.0);
    let v = match d.kind.as_str() {
        "normal" => rng.normal(d.mean.unwrap_or((lo + hi) * 0.5), d.std.unwrap_or(10.0)),
        "lognormal" => rng.lognormal_median(d.mean.unwrap_or(50.0), d.std.unwrap_or(0.5)),
        "poisson" => rng.poisson(d.mean.unwrap_or(5.0)) as f64,
        "zipf" => {
            let span = ((hi - lo).round().max(1.0)) as u64;
            lo + (rng.zipf_index(span, d.s.unwrap_or(1.2)) as f64)
        }
        _ => rng.range_i64(lo as i64, hi as i64) as f64,
    };
    (v.round() as i64).clamp(lo as i64, hi as i64)
}

#[inline]
fn sample_dist_f64(rng: &mut SeekableRng, d: &DistSpec) -> f64 {
    let lo = d.min.unwrap_or(0.0);
    let hi = d.max.unwrap_or(1000.0);
    let v = match d.kind.as_str() {
        "normal" => rng.normal(d.mean.unwrap_or((lo + hi) * 0.5), d.std.unwrap_or(15.0)),
        "lognormal" => rng.lognormal_median(d.mean.unwrap_or(100.0), d.std.unwrap_or(0.6)),
        _ => rng.uniform_f64(lo, hi),
    };
    v.clamp(lo, hi)
}

fn eval_numeric_field_f64(
    schema: &CompiledSchema,
    seed: u64,
    entity_idx: usize,
    row_idx: u64,
    op: &DerivedOperand,
    depth: u32,
) -> f64 {
    if depth > 6 {
        return 0.0;
    }
    match op {
        DerivedOperand::Constant(c) => *c,
        DerivedOperand::Field(fname) => {
            let ent = &schema.entities[entity_idx];
            let Some(f) = ent.fields.iter().find(|x| x.name == *fname) else {
                return 0.0;
            };
            let mut rng =
                SeekableRng::for_field_hashes(seed, ent.name_hash, row_idx, f.name_hash);
            match &f.kind {
                CompiledFieldKind::Value(CompiledValueGenerator::IntDist(d)) => {
                    sample_dist_i64(&mut rng, d) as f64
                }
                CompiledFieldKind::Value(CompiledValueGenerator::FloatDist(d)) => {
                    let v = sample_dist_f64(&mut rng, d);
                    let dec = d.decimals.unwrap_or(2) as i32;
                    let factor = 10f64.powi(dec);
                    (v * factor).round() / factor
                }
                CompiledFieldKind::Derived(df) => {
                    eval_derived_numeric(schema, seed, entity_idx, row_idx, df, depth + 1)
                }
                _ => 0.0,
            }
        }
    }
}

fn eval_derived_numeric(
    schema: &CompiledSchema,
    seed: u64,
    entity_idx: usize,
    row_idx: u64,
    df: &DerivedFormula,
    depth: u32,
) -> f64 {
    let l = eval_numeric_field_f64(schema, seed, entity_idx, row_idx, &df.lhs, depth);
    let r = eval_numeric_field_f64(schema, seed, entity_idx, row_idx, &df.rhs, depth);
    match df.op {
        DerivedOp::Add => l + r,
        DerivedOp::Sub => l - r,
        DerivedOp::Mul => l * r,
        DerivedOp::Div => {
            if r.abs() < 1e-12 {
                0.0
            } else {
                l / r
            }
        }
        DerivedOp::Gt => {
            if l > r {
                1.0
            } else {
                0.0
            }
        }
        DerivedOp::Ge => {
            if l >= r {
                1.0
            } else {
                0.0
            }
        }
        DerivedOp::Lt => {
            if l < r {
                1.0
            } else {
                0.0
            }
        }
        DerivedOp::Le => {
            if l <= r {
                1.0
            } else {
                0.0
            }
        }
        DerivedOp::Eq => {
            if (l - r).abs() < 1e-9 {
                1.0
            } else {
                0.0
            }
        }
    }
}

/// Format a single field value into `val_buf` and return whether it is numeric/boolean (for unquoted JSON).
#[inline]
pub fn eval_field_into(
    schema: &CompiledSchema,
    seed: u64,
    entity_idx: usize,
    row_idx: u64,
    field: &CompiledField,
    val_buf: &mut String,
) -> bool {
    val_buf.clear();
    let ent = &schema.entities[entity_idx];
    match &field.kind {
        CompiledFieldKind::PrimaryKey(pk) => {
            pk.write_pk(seed, ent.name_hash, row_idx, val_buf);
            false
        }
        CompiledFieldKind::ForeignKey {
            target_entity_idx,
            dist,
            ..
        } => {
            let parent_idx = eval_fk_target_index(
                schema,
                seed,
                ent,
                row_idx,
                field,
                *target_entity_idx,
                dist,
            );
            let target_ent = &schema.entities[*target_entity_idx];
            target_ent
                .pk_kind
                .write_pk(seed, target_ent.name_hash, parent_idx, val_buf);
            false
        }
        CompiledFieldKind::ForeignKeyList {
            target_entity_idx,
            min_items,
            max_items,
            dist,
            ..
        } => {
            let mut rng =
                SeekableRng::for_field_hashes(seed, ent.name_hash, row_idx, field.name_hash);
            let count = rng.range_i64(*min_items as i64, *max_items as i64) as usize;
            let target_ent = &schema.entities[*target_entity_idx];
            let target_count = target_ent.count.max(1);
            for k in 0..count {
                if k > 0 {
                    val_buf.push(';');
                }
                let p_idx = match dist {
                    FkDist::Uniform => rng.integers(target_count),
                    FkDist::Zipf { s } => rng.zipf_index(target_count, *s),
                };
                target_ent
                    .pk_kind
                    .write_pk(seed, target_ent.name_hash, p_idx, val_buf);
            }
            false
        }
        CompiledFieldKind::Enum {
            values,
            cumulative_weights,
            ..
        } => {
            let mut rng =
                SeekableRng::for_field_hashes(seed, ent.name_hash, row_idx, field.name_hash);
            if let Some(cum) = cumulative_weights {
                if cum.len() == values.len() {
                    let u = rng.next_f64();
                    let idx = cum
                        .iter()
                        .position(|&c| u <= c)
                        .unwrap_or(values.len().saturating_sub(1));
                    val_buf.push_str(&values[idx]);
                    return false;
                }
            }
            val_buf.push_str(rng.choose(values));
            false
        }
        CompiledFieldKind::TemporalAfter { .. } => {
            let days = eval_field_epoch_days(schema, seed, entity_idx, row_idx, &field.name, 0);
            format_date_from_epoch_days(days, val_buf);
            false
        }
        CompiledFieldKind::Derived(df) => {
            let v = eval_derived_numeric(schema, seed, entity_idx, row_idx, df, 0);
            if df.op.is_comparison() {
                if v > 0.5 {
                    val_buf.push_str("true");
                } else {
                    val_buf.push_str("false");
                }
                true
            } else if df.decimals == 0 {
                let mut b = itoa::Buffer::new();
                val_buf.push_str(b.format(v.round() as i64));
                true
            } else {
                let _ = write!(val_buf, "{:.*}", df.decimals, v);
                true
            }
        }
        CompiledFieldKind::Value(gen) => {
            let mut rng =
                SeekableRng::for_field_hashes(seed, ent.name_hash, row_idx, field.name_hash);
            match gen {
                CompiledValueGenerator::Regex(re) => {
                    re.generate_into(&mut rng, val_buf);
                    false
                }
                CompiledValueGenerator::Pool(pool) => {
                    val_buf.push_str(rng.choose(pool));
                    false
                }
                CompiledValueGenerator::FullName { first, last } => {
                    val_buf.push_str(rng.choose(first));
                    val_buf.push(' ');
                    val_buf.push_str(rng.choose(last));
                    false
                }
                CompiledValueGenerator::IntDist(d) => {
                    let v = sample_dist_i64(&mut rng, d);
                    let mut b = itoa::Buffer::new();
                    val_buf.push_str(b.format(v));
                    true
                }
                CompiledValueGenerator::FloatDist(d) => {
                    let v = sample_dist_f64(&mut rng, d);
                    let dec = d.decimals.unwrap_or(2) as usize;
                    let _ = write!(val_buf, "{:.*}", dec, v);
                    true
                }
                CompiledValueGenerator::DateDays { min_day, max_day } => {
                    let days = rng.range_i64(*min_day, *max_day);
                    format_date_from_epoch_days(days, val_buf);
                    false
                }
                CompiledValueGenerator::TimeOfDay { min_sec, max_sec } => {
                    let sec = rng.range_i64(*min_sec as i64, *max_sec as i64) as u32;
                    let hh = sec / 3600;
                    let mm = (sec % 3600) / 60;
                    let ss = sec % 60;
                    let _ = write!(val_buf, "{:02}:{:02}:{:02}", hh, mm, ss);
                    false
                }
                CompiledValueGenerator::Bool { true_prob } => {
                    if rng.next_f64() < *true_prob {
                        val_buf.push_str("true");
                    } else {
                        val_buf.push_str("false");
                    }
                    true
                }
            }
        }
    }
}

pub fn execute_schema(
    schema: &CompiledSchema,
    seed: u64,
    out_dir: &Path,
    format: OutputFormat,
    scale_factor: f64,
    schema_path: Option<&Path>,
    dict_path: Option<&Path>,
) -> Result<SchemaManifest> {
    let t0 = Instant::now();
    fs::create_dir_all(out_dir)?;

    let emit_csv = matches!(format, OutputFormat::Csv | OutputFormat::Both);
    let emit_graph = matches!(format, OutputFormat::Graph | OutputFormat::Both);

    let mut nodes_writer = if emit_graph {
        Some(HashingWriter::create(&out_dir.join("nodes.jsonl"))?)
    } else {
        None
    };
    let mut edges_writer = if emit_graph {
        Some(HashingWriter::create(&out_dir.join("edges.jsonl"))?)
    } else {
        None
    };

    let mut outputs = BTreeMap::new();
    let mut counts = SchemaCounts::default();
    let mut total_records = 0u64;
    let mut total_edges = 0u64;

    // Execute entities in topological order (parents before children)
    for &entity_idx in &schema.topo_order {
        let ent = &schema.entities[entity_idx];
        counts.entities.insert(ent.name.clone(), ent.count);
        total_records += ent.count;

        let csv_file_name = format!("{}.csv", ent.name);
        let mut csv_writer = if emit_csv {
            let mut w = HashingWriter::create(&out_dir.join(&csv_file_name))?;
            // Write CSV header
            let header: String = ent
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
                + "\n";
            w.write_all(header.as_bytes())?;
            Some(w)
        } else {
            None
        };

        let num_chunks = ent.count.div_ceil(CHUNK_ROWS);
        let mut chunk_start = 0u64;

        while chunk_start < num_chunks {
            let chunk_end = (chunk_start + WINDOW_CHUNKS).min(num_chunks);
            let chunk_indices: Vec<u64> = (chunk_start..chunk_end).collect();

            let rendered: Vec<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<(String, u64)>)> = chunk_indices
                .into_par_iter()
                .map(|c_idx| {
                    let row_lo = c_idx * CHUNK_ROWS;
                    let row_hi = ((c_idx + 1) * CHUNK_ROWS).min(ent.count);
                    let cap = ((row_hi - row_lo) as usize) * 80;
                    let mut csv_buf = if emit_csv {
                        Vec::with_capacity(cap)
                    } else {
                        Vec::new()
                    };
                    let mut nodes_buf = if emit_graph && ent.edge_spec.is_none() {
                        Vec::with_capacity(cap)
                    } else {
                        Vec::new()
                    };
                    let mut edges_buf = if emit_graph {
                        Vec::with_capacity(cap / 2)
                    } else {
                        Vec::new()
                    };
                    let mut rel_counts: BTreeMap<String, u64> = BTreeMap::new();

                    let mut val_buf = String::with_capacity(64);
                    let mut pk_buf = String::with_capacity(36);
                    let mut edge_props_buf = Vec::with_capacity(128);
                    let mut edge_src_buf = String::with_capacity(36);
                    let mut edge_dst_buf = String::with_capacity(36);

                    for row_idx in row_lo..row_hi {
                        pk_buf.clear();
                        ent.pk_kind
                            .write_pk(seed, ent.name_hash, row_idx, &mut pk_buf);

                        if let Some(edge_spec) = &ent.edge_spec {
                            // First-class attributed property-graph edge entity (`@edge(from: ..., to: ..., type: ...)`)
                            edge_props_buf.clear();
                            edge_src_buf.clear();
                            edge_dst_buf.clear();

                            for (f_idx, field) in ent.fields.iter().enumerate() {
                                let is_raw_json = eval_field_into(
                                    schema,
                                    seed,
                                    entity_idx,
                                    row_idx,
                                    field,
                                    &mut val_buf,
                                );
                                if emit_csv {
                                    if f_idx > 0 {
                                        csv_buf.push(b',');
                                    }
                                    csv_buf.extend_from_slice(val_buf.as_bytes());
                                }
                                if emit_graph {
                                    if f_idx == edge_spec.from_field_idx {
                                        edge_src_buf.push_str(&val_buf);
                                    } else if f_idx == edge_spec.to_field_idx {
                                        edge_dst_buf.push_str(&val_buf);
                                    } else if !matches!(field.kind, CompiledFieldKind::PrimaryKey(_))
                                    {
                                        edge_props_buf.extend_from_slice(b",\"");
                                        edge_props_buf.extend_from_slice(field.name.as_bytes());
                                        edge_props_buf.extend_from_slice(b"\":");
                                        if is_raw_json {
                                            edge_props_buf.extend_from_slice(val_buf.as_bytes());
                                        } else {
                                            edge_props_buf.push(b'"');
                                            edge_props_buf.extend_from_slice(val_buf.as_bytes());
                                            edge_props_buf.push(b'"');
                                        }
                                    }
                                }
                            }
                            if emit_csv {
                                csv_buf.push(b'\n');
                            }
                            if emit_graph {
                                edges_buf.extend_from_slice(b"{\"id\":\"");
                                edges_buf.extend_from_slice(pk_buf.as_bytes());
                                edges_buf.extend_from_slice(b"\",\"src\":\"");
                                edges_buf.extend_from_slice(edge_src_buf.as_bytes());
                                edges_buf.extend_from_slice(b"\",\"rel\":\"");
                                edges_buf.extend_from_slice(edge_spec.edge_label.as_bytes());
                                edges_buf.extend_from_slice(b"\",\"dst\":\"");
                                edges_buf.extend_from_slice(edge_dst_buf.as_bytes());
                                edges_buf.push(b'"');
                                edges_buf.extend_from_slice(&edge_props_buf);
                                edges_buf.extend_from_slice(b",\"synthetic\":true}\n");
                                *rel_counts
                                    .entry(edge_spec.edge_label.clone())
                                    .or_insert(0) += 1;
                            }
                            continue;
                        }

                        if emit_graph {
                            nodes_buf.extend_from_slice(b"{\"id\":\"");
                            nodes_buf.extend_from_slice(pk_buf.as_bytes());
                            nodes_buf.extend_from_slice(b"\",\"label\":\"");
                            nodes_buf.extend_from_slice(ent.name.as_bytes());
                            nodes_buf.extend_from_slice(b"\"");
                        }

                        for (f_idx, field) in ent.fields.iter().enumerate() {
                            let is_raw_json = eval_field_into(
                                schema,
                                seed,
                                entity_idx,
                                row_idx,
                                field,
                                &mut val_buf,
                            );

                            if emit_csv {
                                if f_idx > 0 {
                                    csv_buf.push(b',');
                                }
                                csv_buf.extend_from_slice(val_buf.as_bytes());
                            }

                            if emit_graph {
                                match &field.kind {
                                    CompiledFieldKind::PrimaryKey(_) => {}
                                    CompiledFieldKind::ForeignKey { edge_label, .. } => {
                                        edges_buf.extend_from_slice(b"{\"src\":\"");
                                        edges_buf.extend_from_slice(pk_buf.as_bytes());
                                        edges_buf.extend_from_slice(b"\",\"rel\":\"");
                                        edges_buf.extend_from_slice(edge_label.as_bytes());
                                        edges_buf.extend_from_slice(b"\",\"dst\":\"");
                                        edges_buf.extend_from_slice(val_buf.as_bytes());
                                        edges_buf.extend_from_slice(b"\"}\n");
                                        *rel_counts.entry(edge_label.clone()).or_insert(0) += 1;
                                    }
                                    CompiledFieldKind::ForeignKeyList { edge_label, .. } => {
                                        for target_pk in val_buf.split(';') {
                                            if !target_pk.is_empty() {
                                                edges_buf.extend_from_slice(b"{\"src\":\"");
                                                edges_buf.extend_from_slice(pk_buf.as_bytes());
                                                edges_buf.extend_from_slice(b"\",\"rel\":\"");
                                                edges_buf.extend_from_slice(edge_label.as_bytes());
                                                edges_buf.extend_from_slice(b"\",\"dst\":\"");
                                                edges_buf.extend_from_slice(target_pk.as_bytes());
                                                edges_buf.extend_from_slice(b"\"}\n");
                                                *rel_counts
                                                    .entry(edge_label.clone())
                                                    .or_insert(0) += 1;
                                            }
                                        }
                                    }
                                    _ => {
                                        nodes_buf.extend_from_slice(b",\"");
                                        nodes_buf.extend_from_slice(field.name.as_bytes());
                                        nodes_buf.extend_from_slice(b"\":");
                                        if is_raw_json {
                                            nodes_buf.extend_from_slice(val_buf.as_bytes());
                                        } else {
                                            nodes_buf.push(b'"');
                                            nodes_buf.extend_from_slice(val_buf.as_bytes());
                                            nodes_buf.push(b'"');
                                        }
                                    }
                                }
                            }
                        }

                        if emit_csv {
                            csv_buf.push(b'\n');
                        }
                        if emit_graph {
                            nodes_buf.extend_from_slice(b",\"synthetic\":true}\n");
                        }
                    }

                    (
                        csv_buf,
                        nodes_buf,
                        edges_buf,
                        rel_counts.into_iter().collect(),
                    )
                })
                .collect();

            for (csv_buf, nodes_buf, edges_buf, rels) in rendered {
                if let Some(w) = &mut csv_writer {
                    w.write_all(&csv_buf)?;
                }
                if let Some(w) = &mut nodes_writer {
                    w.write_all(&nodes_buf)?;
                }
                if let Some(w) = &mut edges_writer {
                    w.write_all(&edges_buf)?;
                }
                for (r, cnt) in rels {
                    *counts.relations.entry(r).or_insert(0) += cnt;
                    total_edges += cnt;
                }
            }

            chunk_start = chunk_end;
        }

        if let Some(w) = csv_writer {
            let sha = w.finish()?;
            outputs.insert(csv_file_name, sha);
        }
    }

    if let Some(w) = nodes_writer {
        outputs.insert("nodes.jsonl".to_string(), w.finish()?);
    }
    if let Some(w) = edges_writer {
        outputs.insert("edges.jsonl".to_string(), w.finish()?);
    }

    if let Some(b) = &schema.binding {
        let binding_path = out_dir.join("binding.json");
        let json = serde_json::to_string_pretty(b)?;
        fs::write(&binding_path, json)?;
        outputs.insert("binding.json".to_string(), sha256_file(&binding_path)?);
    }

    let mut inputs = BTreeMap::new();
    if let Some(sp) = schema_path {
        if sp.exists() {
            inputs.insert(
                sp.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("schema.graphql")
                    .to_string(),
                sha256_file(sp)?,
            );
        }
    }
    if let Some(dp) = dict_path {
        if dp.exists() {
            inputs.insert(
                dp.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("dictionary.yaml")
                    .to_string(),
                sha256_file(dp)?,
            );
        }
    }
    if let Some(b) = &schema.binding {
        for (k, v) in &b.inputs {
            inputs.insert(k.clone(), v.clone());
        }
    }

    let duration_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let records_per_sec = if duration_ms > 0.0 {
        (total_records as f64) / (duration_ms / 1000.0)
    } else {
        0.0
    };

    let manifest = SchemaManifest {
        generator: format!("samyama-sdg {}", env!("CARGO_PKG_VERSION")),
        mode: format!("declarative-{:?}", schema.frontend).to_lowercase(),
        seed,
        scale_factor,
        format: format!("{:?}", format).to_lowercase(),
        schema_file: schema_path
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .map(String::from),
        dictionary_file: dict_path
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .map(String::from),
        inputs,
        outputs,
        counts,
        total_records,
        total_edges,
        duration_ms: (duration_ms * 100.0).round() / 100.0,
        records_per_sec: records_per_sec.round(),
    };

    let manifest_path = out_dir.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;

    Ok(manifest)
}
