use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use samyama_sdg::dictionary::DataDictionary;
use samyama_sdg::engine::{execute_schema, OutputFormat};
use samyama_sdg::schema::CompiledSchema;
use samyama_sdg::telecom::{generate_telecom, validate_telecom, TelecomConfig};
use samyama_sdg::validate_schema::validate_schema_output;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

const DEFAULT_SEED: u64 = 20261019;

#[derive(Parser)]
#[command(
    name = "sdg-rs",
    version,
    about = "Samyama Synthetic Data Compiler: GraphQL @count + Regex/Ontology Data Dictionary & Causal Simulator"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile and execute a declarative GraphQL @count(N) schema + Data Dictionary
    Schema {
        /// Path to the input .graphql schema file
        schema: PathBuf,
        /// Output directory for generated CSV / JSONL files
        #[arg(long)]
        out: PathBuf,
        /// Optional YAML/JSON Data Dictionary file overriding or extending built-in attribute rules
        #[arg(long)]
        dictionary: Option<PathBuf>,
        /// Optional samyama-ontologies checkout or build directory
        #[arg(long)]
        ontologies: Option<PathBuf>,
        /// Master deterministic seed
        #[arg(long, default_value_t = DEFAULT_SEED)]
        seed: u64,
        /// Scale multiplier applied to all @count(N) directives
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        /// Override specific entity counts (repeatable: --count Patient=100000)
        #[arg(long = "count", value_name = "ENTITY=N")]
        counts: Vec<String>,
        /// Output format: csv, graph, or both
        #[arg(long, default_value = "both")]
        format: String,
        /// Run conformance, referential integrity, regex & temporal validation after generation
        #[arg(long)]
        validate: bool,
    },
    /// Validate a generated schema dataset directory against its .graphql specification
    ValidateSchema {
        /// Path to the .graphql schema file
        schema: PathBuf,
        /// Directory containing generated files
        dir: PathBuf,
        /// Optional Data Dictionary file
        #[arg(long)]
        dictionary: Option<PathBuf>,
        /// Optional samyama-ontologies directory
        #[arg(long)]
        ontologies: Option<PathBuf>,
        /// Scale factor used during generation
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
    },
    /// Generate a dataset from a domain pack config (e.g. configs/telecom/small.yaml)
    Generate {
        /// Path to pack YAML config
        config: PathBuf,
        /// Output directory
        #[arg(long)]
        out: PathBuf,
        /// Deterministic seed
        #[arg(long, default_value_t = DEFAULT_SEED)]
        seed: u64,
        /// samyama-ontologies checkout directory
        #[arg(long)]
        ontologies: Option<PathBuf>,
        /// Override topology path
        #[arg(long)]
        topology: Option<PathBuf>,
    },
    /// Validate a telecom pack dataset directory
    Validate {
        /// Dataset directory
        dir: PathBuf,
    },
    /// Run multi-scale throughput and memory benchmark suite
    Bench {
        /// Path to .graphql schema file to benchmark
        schema: PathBuf,
        /// Output JSON file for benchmark results
        #[arg(long)]
        out_json: Option<PathBuf>,
        /// Format to benchmark: csv, graph, or both
        #[arg(long, default_value = "csv")]
        format: String,
    },
}

fn read_vm_hwm_kib() -> u64 {
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                if let Some(num) = rest.split_whitespace().next() {
                    if let Ok(kib) = num.parse::<u64>() {
                        return kib;
                    }
                }
            }
        }
    }
    0
}

fn resolve_ontologies_build(opt: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(p) = opt {
        if p.join("build").exists() {
            return Some(p.join("build"));
        }
        return Some(p);
    }
    let sibling = PathBuf::from("../samyama-ontologies/build");
    if sibling.exists() {
        return Some(sibling);
    }
    let fixture = PathBuf::from("tests/fixtures/ontology-build");
    if fixture.exists() {
        return Some(fixture);
    }
    None
}

fn parse_count_overrides(raw: &[String]) -> Result<BTreeMap<String, u64>> {
    let mut map = BTreeMap::new();
    for item in raw {
        let (k, v) = item
            .split_once('=')
            .ok_or_else(|| anyhow!("--count expects ENTITY=N, got {:?}", item))?;
        let n: u64 = v.replace('_', "").parse()?;
        map.insert(k.trim().to_string(), n);
    }
    Ok(map)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Schema {
            schema,
            out,
            dictionary,
            ontologies,
            seed,
            scale,
            counts,
            format,
            validate,
        } => {
            let mut dict = DataDictionary::built_in()?;
            if let Some(dp) = &dictionary {
                dict.merge_yaml_file(dp)?;
            }
            let onto_build = resolve_ontologies_build(ontologies);
            let overrides = parse_count_overrides(&counts)?;
            let compiled = CompiledSchema::compile_file(
                &schema,
                dict,
                onto_build.as_deref(),
                scale,
                &overrides,
            )?;
            let fmt = OutputFormat::parse(&format)?;
            let manifest = execute_schema(
                &compiled,
                seed,
                &out,
                fmt,
                scale,
                Some(&schema),
                dictionary.as_deref(),
            )?;

            if validate {
                let rep = validate_schema_output(&compiled, &out)?;
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "manifest": {
                            "total_records": manifest.total_records,
                            "total_edges": manifest.total_edges,
                            "duration_ms": manifest.duration_ms,
                            "records_per_sec": manifest.records_per_sec,
                            "entities": manifest.counts.entities,
                        },
                        "validation": rep.summary
                    }))?
                );
                if rep.summary.conformance_errors > 0 {
                    std::process::exit(1);
                }
            } else {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "total_records": manifest.total_records,
                        "total_edges": manifest.total_edges,
                        "duration_ms": manifest.duration_ms,
                        "records_per_sec": manifest.records_per_sec,
                        "entities": manifest.counts.entities,
                    }))?
                );
            }
        }
        Command::ValidateSchema {
            schema,
            dir,
            dictionary,
            ontologies,
            scale,
        } => {
            let mut dict = DataDictionary::built_in()?;
            if let Some(dp) = &dictionary {
                dict.merge_yaml_file(dp)?;
            }
            let onto_build = resolve_ontologies_build(ontologies);
            let compiled = CompiledSchema::compile_file(
                &schema,
                dict,
                onto_build.as_deref(),
                scale,
                &BTreeMap::new(),
            )?;
            let rep = validate_schema_output(&compiled, &dir)?;
            eprintln!("{}", serde_json::to_string_pretty(&rep.summary)?);
            if rep.summary.conformance_errors > 0 {
                std::process::exit(1);
            }
        }
        Command::Generate {
            config,
            out,
            seed,
            ontologies,
            topology,
        } => {
            let raw = fs::read_to_string(&config)?;
            let mut cfg: TelecomConfig = serde_yaml::from_str(&raw)?;
            if let Some(tp) = topology {
                cfg.topology = tp.to_string_lossy().to_string();
            }
            if !Path::new(&cfg.topology).exists() {
                if let Some(fname) = Path::new(&cfg.topology).file_name() {
                    let candidate = PathBuf::from("tests/fixtures").join(fname);
                    if candidate.exists() {
                        cfg.topology = candidate.to_string_lossy().to_string();
                    }
                }
            }
            let onto_build = resolve_ontologies_build(ontologies)
                .ok_or_else(|| anyhow!("could not locate samyama-ontologies build directory"))?;
            let m = generate_telecom(&cfg, seed, &onto_build, &out, Some(&config))?;
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "faults": m["faults"],
                    "alarms": m["alarms"],
                    "incidents": m["incidents"],
                    "synthetic_share": m["synthetic_share"],
                    "counts": m["counts"]
                }))?
            );
        }
        Command::Validate { dir } => {
            let rep = validate_telecom(&dir)?;
            eprintln!("{}", serde_json::to_string_pretty(&rep["summary"])?);
            if rep["summary"]["conformance_errors"].as_u64().unwrap_or(1) > 0 {
                std::process::exit(1);
            }
        }
        Command::Bench {
            schema,
            out_json,
            format,
        } => {
            let sdl = fs::read_to_string(&schema)?;
            let fmt = OutputFormat::parse(&format)?;
            let scales = [1.0, 10.0, 100.0, 1_000.0, 5_000.0];
            let mut runs = Vec::new();
            let tmp_base = std::env::temp_dir().join("samyama-sdg-bench");

            for &scale in &scales {
                let dict = DataDictionary::built_in()?;
                let compiled =
                    CompiledSchema::compile(&sdl, dict, None, scale, &BTreeMap::new())?;
                let run_dir = tmp_base.join(format!("scale_{}", scale as u64));
                let _ = fs::remove_dir_all(&run_dir);
                let t0 = Instant::now();
                let manifest = execute_schema(
                    &compiled,
                    DEFAULT_SEED,
                    &run_dir,
                    fmt,
                    scale,
                    Some(&schema),
                    None,
                )?;
                let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
                let hwm_kib = read_vm_hwm_kib();
                let _ = fs::remove_dir_all(&run_dir);
                runs.push(json!({
                    "scale_factor": scale,
                    "total_records": manifest.total_records,
                    "total_edges": manifest.total_edges,
                    "duration_ms": (elapsed_ms * 100.0).round() / 100.0,
                    "records_per_sec": ((manifest.total_records as f64) / (elapsed_ms / 1000.0)).round(),
                    "peak_rss_kib": hwm_kib,
                }));
            }
            let report = json!({
                "schema": schema.file_name().and_then(|s| s.to_str()),
                "format": format,
                "runs": runs,
            });
            let pretty = serde_json::to_string_pretty(&report)?;
            println!("{}", pretty);
            if let Some(out_p) = out_json {
                fs::write(out_p, &pretty)?;
            }
        }
    }
    Ok(())
}
