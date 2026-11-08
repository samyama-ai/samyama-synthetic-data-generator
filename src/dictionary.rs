//! Global Attribute Data Dictionary.
//!
//! Implements the Data Dictionary specification from the April 2023 *Consistent Synthetic Data
//! Generator* design, upgraded with parametric distributions and ontology-bound enumerations:
//! - Maps scalar type names (`Email`, `Phone`, `Date`, `Time`, `ICD10`, `CPT`, `NPI`, `SKU`,
//!   `Money`, `IPv4`) and common attribute names (`firstName`, `lastName`, `email`, `age`,
//!   `price`, `grade`, `status`) to compiled regex generators, realistic vocabulary pools,
//!   or statistical distributions so no attribute ever falls back to unrecognizable
//!   *Lorem ipsum* or random hex noise.

use crate::regex_gen::CompiledRegex;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DistSpec {
    /// `"uniform"` | `"normal"` | `"lognormal"` | `"zipf"` | `"poisson"`
    pub kind: String,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub mean: Option<f64>,
    #[serde(default)]
    pub std: Option<f64>,
    #[serde(default)]
    pub s: Option<f64>,
    #[serde(default)]
    pub decimals: Option<u32>,
}

impl Default for DistSpec {
    fn default() -> Self {
        Self {
            kind: "uniform".to_string(),
            min: Some(0.0),
            max: Some(100.0),
            mean: None,
            std: None,
            s: None,
            decimals: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttributeRuleSpec {
    #[serde(default)]
    pub regex: Option<String>,
    #[serde(default)]
    pub values: Option<Vec<String>>,
    #[serde(default)]
    pub dist: Option<DistSpec>,
    /// Date range in days since 1970-01-01 (e.g., `1950-01-01` = `-7305`, `2026-01-01` = `20454`)
    #[serde(default)]
    pub date_range: Option<(i64, i64)>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DataDictionaryFile {
    #[serde(default)]
    pub scalars: BTreeMap<String, AttributeRuleSpec>,
    #[serde(default)]
    pub attributes: BTreeMap<String, AttributeRuleSpec>,
}

#[derive(Clone, Debug)]
pub enum CompiledValueGenerator {
    Regex(CompiledRegex),
    Pool(Vec<String>),
    FullName {
        first: Vec<String>,
        last: Vec<String>,
    },
    IntDist(DistSpec),
    FloatDist(DistSpec),
    /// Days since Unix epoch `1970-01-01` in `[min_day, max_day]`
    DateDays {
        min_day: i64,
        max_day: i64,
    },
    /// Seconds since midnight `[0, 86400)` formatted as `HH:MM:SS`
    TimeOfDay {
        min_sec: u32,
        max_sec: u32,
    },
    Bool {
        true_prob: f64,
    },
}

#[derive(Clone, Debug)]
pub struct DataDictionary {
    pub scalars: BTreeMap<String, CompiledValueGenerator>,
    pub attributes: BTreeMap<String, CompiledValueGenerator>,
}

const FIRST_NAMES: &[&str] = &[
    "Aarav", "Aditi", "Alejandro", "Amara", "Ananya", "Arjun", "Beatriz", "Camila", "Chen",
    "Clara", "Daniel", "Deepika", "divya", "Elena", "Ethan", "Fatima", "Gabriel", "Hannah",
    "Hiroshi", "Ibrahim", "Ishaan", "Julia", "Karthik", "Kavya", "Liam", "Lucas", "Madhavi",
    "Maya", "Meera", "Mohammed", "Nadia", "Nikhil", "Noah", "Olivia", "Priya", "rahul",
    "Rohan", "Saanvi", "Sandeep", "Sara", "Siddharth", "Sofia", "Tariq", "Vikram", "Yuki", "Zoe",
];

const LAST_NAMES: &[&str] = &[
    "Agarwal", "Alvarez", "Banerjee", "Chatterjee", "Chen", "Das", "Deshmukh", "Fernandez",
    "Garcia", "Gupta", "Iyer", "Joshi", "Kapoor", "Khan", "Kulkarni", "Kumar", "Li", "Mandarapu",
    "Mehra", "menon", "Mishra", "Moreno", "Murthy", "Nair", "Nguyen", "Okafor", "Patel", "Rao",
    "Reddy", "Rodriguez", "Sato", "Sharma", "Singh", "Srinivasan", "Subramanian", "Suzuki",
    "Takahashi", "Varma", "Verma", "Wang", "Williams", "Zhang",
];

const PRODUCT_NAMES: &[&str] = &[
    "Optical Transceiver SFP28",
    "Core Router Line Card 100G",
    "Rack Power Distribution Unit",
    "Fiber Patch Cord LC-LC 10m",
    "Managed Edge Switch 48P",
    "GNSS Timing Receiver Module",
    "Carrier Grade NAT Appliance",
    "Telemetry Collector Node",
    "Precision Glucometer Kit",
    "Digital Pulse Oximeter Pro",
    "Automated Infusion Pump",
    "Portable ECG Monitor 12-Lead",
    "Wireless Blood Pressure Cuff",
    "Smart Dosage Dispenser Hub",
    "Cloud Analytics Enterprise Seat",
    "Zero-Trust Hardware Security Token",
];

const COURSE_NAMES: &[&str] = &[
    "Distributed Systems and Consensus",
    "Database Internals and Query Compilers",
    "Graph Algorithms and Network Science",
    "Statistical Machine Learning",
    "Compilers and Intermediate Representations",
    "Computer Networks and Routing Protocols",
    "Biomedical Informatics and Clinical Data",
    "Cryptography and Zero-Knowledge Proofs",
    "Operating Systems Kernel Design",
    "Stochastic Processes and Queueing Theory",
    "High-Performance Computing on GPUs",
    "Formal Verification and Model Checking",
];

const DEPARTMENT_NAMES: &[&str] = &[
    "Computer Science",
    "Electrical Engineering",
    "Biomedical Data Science",
    "Applied Mathematics",
    "Operations Research",
    "Information Systems",
    "Computational Biology",
    "Telecommunications Engineering",
];

impl DataDictionary {
    /// Construct the default built-in Data Dictionary with recognizable semantic generators.
    pub fn built_in() -> Result<Self> {
        let mut dict = Self {
            scalars: BTreeMap::new(),
            attributes: BTreeMap::new(),
        };

        // Built-in Scalars
        dict.scalars.insert(
            "Email".to_string(),
            CompiledValueGenerator::Regex(CompiledRegex::compile(
                r"^[a-z]{5,9}\.[a-z]{4,8}@(example|clinic|enterprise|university)\.(com|org|edu)$",
            )?),
        );
        dict.scalars.insert(
            "Phone".to_string(),
            CompiledValueGenerator::Regex(CompiledRegex::compile(
                r"^\+1-[2-9][0-9]{2}-[2-9][0-9]{2}-[0-9]{4}$",
            )?),
        );
        dict.scalars.insert(
            "ICD10".to_string(),
            CompiledValueGenerator::Pool(
                [
                    "E11.9", "I10", "J45.909", "N18.3", "E78.5", "I25.10", "K21.9", "M54.5",
                    "F41.1", "G47.33", "J06.9", "E03.9",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ),
        );
        dict.scalars.insert(
            "CPT".to_string(),
            CompiledValueGenerator::Pool(
                [
                    "99213", "99214", "83036", "80053", "85025", "93000", "71046", "90834",
                    "36415", "80061",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ),
        );
        dict.scalars.insert(
            "NDC".to_string(),
            CompiledValueGenerator::Regex(CompiledRegex::compile(
                r"^[0-9]{5}-[0-9]{4}-[0-9]{2}$",
            )?),
        );
        dict.scalars.insert(
            "SKU".to_string(),
            CompiledValueGenerator::Regex(CompiledRegex::compile(r"^SKU-[A-Z]{3}-[0-9]{5}$")?),
        );
        dict.scalars.insert(
            "IPv4".to_string(),
            CompiledValueGenerator::Regex(CompiledRegex::compile(
                r"^10\.[1-9][0-9]?\.[1-9][0-9]?\.[1-9][0-9]?$",
            )?),
        );
        dict.scalars.insert(
            "Date".to_string(),
            CompiledValueGenerator::DateDays {
                min_day: 18_262, // 2020-01-01
                max_day: 20_758, // 2026-11-01
            },
        );
        dict.scalars.insert(
            "Time".to_string(),
            CompiledValueGenerator::TimeOfDay {
                min_sec: 8 * 3600,
                max_sec: 18 * 3600,
            },
        );
        dict.scalars.insert(
            "Money".to_string(),
            CompiledValueGenerator::FloatDist(DistSpec {
                kind: "lognormal".to_string(),
                min: Some(15.0),
                max: Some(25_000.0),
                mean: Some(180.0),
                std: Some(0.85),
                s: None,
                decimals: Some(2),
            }),
        );

        let first_pool: Vec<String> = FIRST_NAMES
            .iter()
            .map(|s| {
                let mut c = s.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect();
        let last_pool: Vec<String> = LAST_NAMES
            .iter()
            .map(|s| {
                let mut c = s.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect();

        // Built-in Attribute Name Rules (case-insensitive normalized keys)
        dict.attributes.insert(
            "firstname".to_string(),
            CompiledValueGenerator::Pool(first_pool.clone()),
        );
        dict.attributes.insert(
            "lastname".to_string(),
            CompiledValueGenerator::Pool(last_pool.clone()),
        );
        dict.attributes.insert(
            "name".to_string(),
            CompiledValueGenerator::FullName {
                first: first_pool.clone(),
                last: last_pool.clone(),
            },
        );
        dict.attributes.insert(
            "instructor".to_string(),
            CompiledValueGenerator::FullName {
                first: first_pool,
                last: last_pool,
            },
        );
        dict.attributes.insert(
            "product.name".to_string(),
            CompiledValueGenerator::Pool(PRODUCT_NAMES.iter().map(|s| s.to_string()).collect()),
        );
        dict.attributes.insert(
            "book.title".to_string(),
            CompiledValueGenerator::Pool(COURSE_NAMES.iter().map(|s| s.to_string()).collect()),
        );
        dict.attributes.insert(
            "course.name".to_string(),
            CompiledValueGenerator::Pool(COURSE_NAMES.iter().map(|s| s.to_string()).collect()),
        );
        dict.attributes.insert(
            "department.name".to_string(),
            CompiledValueGenerator::Pool(DEPARTMENT_NAMES.iter().map(|s| s.to_string()).collect()),
        );
        dict.attributes.insert(
            "email".to_string(),
            dict.scalars.get("Email").unwrap().clone(),
        );
        dict.attributes.insert(
            "phone".to_string(),
            dict.scalars.get("Phone").unwrap().clone(),
        );
        dict.attributes.insert(
            "dateofbirth".to_string(),
            CompiledValueGenerator::DateDays {
                min_day: -7_305, // 1950-01-01
                max_day: 12_784, // 2005-01-01
            },
        );
        dict.attributes.insert(
            "age".to_string(),
            CompiledValueGenerator::IntDist(DistSpec {
                kind: "normal".to_string(),
                min: Some(18.0),
                max: Some(92.0),
                mean: Some(48.0),
                std: Some(16.0),
                s: None,
                decimals: None,
            }),
        );
        dict.attributes.insert(
            "price".to_string(),
            dict.scalars.get("Money").unwrap().clone(),
        );
        dict.attributes.insert(
            "amount".to_string(),
            dict.scalars.get("Money").unwrap().clone(),
        );
        dict.attributes.insert(
            "adjudicationamount".to_string(),
            dict.scalars.get("Money").unwrap().clone(),
        );
        dict.attributes.insert(
            "grade".to_string(),
            CompiledValueGenerator::FloatDist(DistSpec {
                kind: "normal".to_string(),
                min: Some(2.0),
                max: Some(4.0),
                mean: Some(3.35),
                std: Some(0.4),
                s: None,
                decimals: Some(2),
            }),
        );
        dict.attributes.insert(
            "quantity".to_string(),
            CompiledValueGenerator::IntDist(DistSpec {
                kind: "zipf".to_string(),
                min: Some(1.0),
                max: Some(20.0),
                mean: None,
                std: None,
                s: Some(1.4),
                decimals: None,
            }),
        );

        Ok(dict)
    }

    pub fn merge_yaml_file(&mut self, path: &Path) -> Result<()> {
        let raw = std::fs::read_to_string(path)?;
        let parsed: DataDictionaryFile = serde_yaml::from_str(&raw)?;
        for (name, rule) in parsed.scalars {
            if let Some(gen) = compile_rule(&rule)? {
                self.scalars.insert(name, gen);
            }
        }
        for (name, rule) in parsed.attributes {
            if let Some(gen) = compile_rule(&rule)? {
                self.attributes.insert(name.to_lowercase(), gen);
            }
        }
        Ok(())
    }

    /// Resolve a generator for `(entity_name, field_name, type_name)`.
    pub fn resolve(
        &self,
        entity_name: &str,
        field_name: &str,
        type_name: &str,
    ) -> Result<CompiledValueGenerator> {
        // 1. Check entity-qualified attribute rule (e.g., `course.name` or `product.name`)
        let qualified = format!("{}.{}", entity_name.to_lowercase(), field_name.to_lowercase());
        if let Some(g) = self.attributes.get(&qualified) {
            return Ok(g.clone());
        }
        // 2. Check custom scalar type name (e.g., `Email`, `Date`, `Time`, `ICD10`, `Money`)
        if let Some(g) = self.scalars.get(type_name) {
            return Ok(g.clone());
        }
        // 3. Check global attribute name (e.g., `firstName`, `lastName`, `email`, `dateOfBirth`, `age`)
        let attr_key = field_name.to_lowercase();
        if let Some(g) = self.attributes.get(&attr_key) {
            return Ok(g.clone());
        }
        // 4. Built-in GraphQL scalar fallbacks
        match type_name {
            "Int" | "Integer" => Ok(CompiledValueGenerator::IntDist(DistSpec {
                kind: "uniform".to_string(),
                min: Some(1.0),
                max: Some(100.0),
                mean: None,
                std: None,
                s: None,
                decimals: None,
            })),
            "Float" | "Double" => Ok(CompiledValueGenerator::FloatDist(DistSpec {
                kind: "uniform".to_string(),
                min: Some(0.0),
                max: Some(1000.0),
                mean: None,
                std: None,
                s: None,
                decimals: Some(2),
            })),
            "Boolean" | "Bool" => Ok(CompiledValueGenerator::Bool { true_prob: 0.5 }),
            _ => {
                // Readable pattern based on field name rather than random noise
                let pat = format!("{}-[A-Z0-9]{{6}}", field_name.to_uppercase());
                Ok(CompiledValueGenerator::Regex(CompiledRegex::compile(&pat)?))
            }
        }
    }
}

pub fn compile_rule(rule: &AttributeRuleSpec) -> Result<Option<CompiledValueGenerator>> {
    if let Some(pat) = &rule.regex {
        return Ok(Some(CompiledValueGenerator::Regex(CompiledRegex::compile(
            pat,
        )?)));
    }
    if let Some(vals) = &rule.values {
        return Ok(Some(CompiledValueGenerator::Pool(vals.clone())));
    }
    if let Some((lo, hi)) = rule.date_range {
        return Ok(Some(CompiledValueGenerator::DateDays {
            min_day: lo,
            max_day: hi,
        }));
    }
    if let Some(dist) = &rule.dist {
        if dist.decimals.is_some() {
            return Ok(Some(CompiledValueGenerator::FloatDist(dist.clone())));
        } else {
            return Ok(Some(CompiledValueGenerator::IntDist(dist.clone())));
        }
    }
    Ok(None)
}

/// Convert days since `1970-01-01` (Howard Hinnant civil-from-days algorithm) to `YYYY-MM-DD`.
#[inline]
pub fn format_date_from_epoch_days(days: i64, out: &mut String) {
    use std::fmt::Write;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    let _ = write!(out, "{:04}-{:02}-{:02}", year, m, d);
}

/// Parse `YYYY-MM-DD` back into days since `1970-01-01` for temporal constraint validation.
pub fn parse_date_to_epoch_days(s: &str) -> Option<i64> {
    let mut parts = s.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = (if m > 2 { m - 3 } else { m + 9 }) as u64;
    let doy = (153 * mp + 2) / 5 + (d as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + (doe as i64) - 719468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_date_roundtrip() {
        for days in [-7305, 0, 18262, 20758] {
            let mut s = String::new();
            format_date_from_epoch_days(days, &mut s);
            let parsed = parse_date_to_epoch_days(&s).expect("valid date");
            assert_eq!(days, parsed, "failed roundtrip for {}", s);
        }
        let mut s = String::new();
        format_date_from_epoch_days(0, &mut s);
        assert_eq!(s, "1970-01-01");
    }
}
