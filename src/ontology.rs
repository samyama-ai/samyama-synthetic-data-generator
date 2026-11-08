//! Bind schema or pack labels and enumerations to `samyama-ontologies` vocabulary builds.
//!
//! Matches `src/sdg/ontology.py` semantics:
//! - Reads `<build>/<vocab>/nodes.jsonl` and `edges.jsonl` and records SHA-256 hashes.
//! - Every referenced concept ID in `labels` must exist in the loaded vocabularies;
//!   an unknown term immediately fails the run (`BindingError`).
//! - Enumerations resolve either by `owner` (`VALUE_OF` edges pointing to the owner node)
//!   or by `scheme` (SKOS concepts whose `scheme` matches).

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .map_err(|e| anyhow!("failed to open {} for sha256: {}", path.display(), e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EnumValueBinding {
    pub value: String,
    pub concept: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Binding {
    pub labels: BTreeMap<String, Vec<String>>,
    pub enums: BTreeMap<String, Vec<EnumValueBinding>>,
    pub local_extensions: Vec<String>,
    pub inputs: BTreeMap<String, String>,
}

impl Binding {
    pub fn values(&self, prop: &str) -> Vec<String> {
        self.enums
            .get(prop)
            .map(|vs| vs.iter().map(|v| v.value.clone()).collect())
            .unwrap_or_default()
    }

    pub fn concept(&self, prop: &str, value: &str) -> Option<String> {
        self.enums
            .get(prop)?
            .iter()
            .find(|v| v.value == value)
            .map(|v| v.concept.clone())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnumBindingSpec {
    pub vocabulary: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub scheme: Option<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BindingSpec {
    pub vocabularies: Vec<String>,
    pub labels: BTreeMap<String, Vec<String>>,
    pub enums: BTreeMap<String, EnumBindingSpec>,
}

struct VocabData {
    nodes: BTreeMap<String, Value>,
    edges: Vec<Value>,
}

fn read_vocab(build: &Path, vocab: &str, inputs: &mut BTreeMap<String, String>) -> Result<VocabData> {
    let dir = build.join(vocab);
    let nodes_path = dir.join("nodes.jsonl");
    let edges_path = dir.join("edges.jsonl");
    if !nodes_path.exists() {
        bail!(
            "no build for {:?} under {}; run samyama-ontologies' converter for it first",
            vocab,
            build.display()
        );
    }
    let mut nodes = BTreeMap::new();
    for line in BufReader::new(File::open(&nodes_path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
            nodes.insert(id.to_string(), v);
        }
    }
    let mut edges = Vec::new();
    for line in BufReader::new(File::open(&edges_path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        edges.push(v);
    }
    inputs.insert(
        format!("{}/nodes.jsonl", vocab),
        sha256_file(&nodes_path)?,
    );
    inputs.insert(
        format!("{}/edges.jsonl", vocab),
        sha256_file(&edges_path)?,
    );
    Ok(VocabData { nodes, edges })
}

pub fn bind(build: &Path, spec: &BindingSpec) -> Result<Binding> {
    let mut b = Binding::default();
    let mut data: HashMap<String, VocabData> = HashMap::new();
    for v in &spec.vocabularies {
        if !data.contains_key(v) {
            let vd = read_vocab(build, v, &mut b.inputs)?;
            data.insert(v.clone(), vd);
        }
    }
    let known: HashSet<&str> = data
        .values()
        .flat_map(|vd| vd.nodes.keys().map(|s| s.as_str()))
        .collect();

    for (label, concepts) in &spec.labels {
        let missing: Vec<&String> = concepts
            .iter()
            .filter(|c| !known.contains(c.as_str()))
            .collect();
        if !missing.is_empty() {
            bail!(
                "label {:?}: concepts not in the vocabularies: {:?}",
                label,
                missing
            );
        }
        if concepts.is_empty() {
            b.local_extensions.push(label.clone());
        }
        b.labels.insert(label.clone(), concepts.clone());
    }

    for (prop, how) in &spec.enums {
        let vd = data.get(&how.vocabulary).ok_or_else(|| {
            anyhow!(
                "enum {:?}: vocabulary {:?} not listed in spec.vocabularies",
                prop,
                how.vocabulary
            )
        })?;
        let ids: Vec<String> = if let Some(owner) = &how.owner {
            if !vd.nodes.contains_key(owner) {
                bail!(
                    "enum {:?}: owner {:?} not in {}",
                    prop,
                    owner,
                    how.vocabulary
                );
            }
            vd.edges
                .iter()
                .filter(|e| {
                    e.get("rel").and_then(|x| x.as_str()) == Some("VALUE_OF")
                        && e.get("dst").and_then(|x| x.as_str()) == Some(owner.as_str())
                })
                .filter_map(|e| e.get("src").and_then(|x| x.as_str()).map(String::from))
                .collect()
        } else if let Some(scheme) = &how.scheme {
            let mut matched: Vec<String> = vd
                .nodes
                .iter()
                .filter(|(_, n)| match n.get("scheme") {
                    Some(Value::String(s)) => s == scheme,
                    Some(Value::Array(arr)) => {
                        arr.iter().any(|x| x.as_str() == Some(scheme.as_str()))
                    }
                    _ => false,
                })
                .map(|(id, _)| id.clone())
                .collect();
            matched.sort();
            matched
        } else {
            bail!("enum {:?}: must specify either owner or scheme", prop);
        };

        let excluded: BTreeSet<&str> = how.exclude.iter().map(|s| s.as_str()).collect();
        let mut seen = HashSet::new();
        let mut vals = Vec::new();
        for id in ids {
            if let Some(node) = vd.nodes.get(&id) {
                if let Some(name) = node.get("name").and_then(|x| x.as_str()) {
                    if seen.contains(name) || excluded.contains(name) {
                        continue;
                    }
                    seen.insert(name.to_string());
                    vals.push(EnumValueBinding {
                        value: name.to_string(),
                        concept: id,
                    });
                }
            }
        }
        if vals.is_empty() {
            bail!(
                "enum {:?}: no values found in {} for {:?}",
                prop,
                how.vocabulary,
                how
            );
        }
        b.enums.insert(prop.clone(), vals);
    }

    Ok(b)
}
