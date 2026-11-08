use samyama_sdg::dictionary::DataDictionary;
use samyama_sdg::engine::{execute_schema, OutputFormat};
use samyama_sdg::schema::CompiledSchema;
use samyama_sdg::telecom::{generate_telecom, validate_telecom, TelecomConfig};
use samyama_sdg::validate_schema::validate_schema_output;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn test_graphql_schema_determinism_and_referential_integrity() {
    let root = repo_root();
    let sdl = fs::read_to_string(root.join("configs/schemas/health_clinic.graphql")).unwrap();
    let mut dict = DataDictionary::built_in().unwrap();
    dict.merge_yaml_file(&root.join("configs/schemas/dictionary.yaml"))
        .unwrap();

    let compiled = CompiledSchema::compile(&sdl, dict, None, 1.0, &BTreeMap::new()).unwrap();

    let tmp = std::env::temp_dir().join("sdg_rust_test_det");
    let _ = fs::remove_dir_all(&tmp);
    let dir_a = tmp.join("run_a");
    let dir_b = tmp.join("run_b");
    let dir_c = tmp.join("run_c");

    let ma = execute_schema(&compiled, 20261019, &dir_a, OutputFormat::Both, 1.0, None, None).unwrap();
    let mb = execute_schema(&compiled, 20261019, &dir_b, OutputFormat::Both, 1.0, None, None).unwrap();
    let mc = execute_schema(&compiled, 20261020, &dir_c, OutputFormat::Both, 1.0, None, None).unwrap();

    assert_eq!(ma.outputs, mb.outputs, "same seed must produce identical file SHA-256s");
    assert_ne!(
        ma.outputs.get("Patient.csv"),
        mc.outputs.get("Patient.csv"),
        "different seed must produce different output"
    );

    let rep = validate_schema_output(&compiled, &dir_a).unwrap();
    assert_eq!(rep.summary.conformance_errors, 0, "errors: {:?}", rep.errors);
    assert_eq!(rep.summary.rows_checked, 1000);
    assert_eq!(rep.summary.foreign_keys_checked, 400 * 2 + 200 + 150);
    assert_eq!(rep.summary.temporal_constraints_checked, 400 + 200 + 150 * 2);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_field_stream_independence_under_schema_evolution() {
    let sdl_v1 = r#"
        type Patient @count(50) @pk(kind: "uuid") {
            id: ID!
            firstName: String!
            email: Email!
            dateOfBirth: Date!
        }
        scalar Email
        scalar Date
    "#;
    let sdl_v2 = r#"
        type Patient @count(50) @pk(kind: "uuid") {
            id: ID!
            firstName: String!
            phone: Phone!
            email: Email!
            dateOfBirth: Date!
        }
        scalar Email
        scalar Phone
        scalar Date
    "#;

    let s1 = CompiledSchema::compile(
        sdl_v1,
        DataDictionary::built_in().unwrap(),
        None,
        1.0,
        &BTreeMap::new(),
    )
    .unwrap();
    let s2 = CompiledSchema::compile(
        sdl_v2,
        DataDictionary::built_in().unwrap(),
        None,
        1.0,
        &BTreeMap::new(),
    )
    .unwrap();

    let tmp = std::env::temp_dir().join("sdg_rust_test_field_indep");
    let _ = fs::remove_dir_all(&tmp);
    let d1 = tmp.join("v1");
    let d2 = tmp.join("v2");
    execute_schema(&s1, 99, &d1, OutputFormat::Csv, 1.0, None, None).unwrap();
    execute_schema(&s2, 99, &d2, OutputFormat::Csv, 1.0, None, None).unwrap();

    let lines1: Vec<String> = fs::read_to_string(d1.join("Patient.csv"))
        .unwrap()
        .lines()
        .skip(1)
        .map(String::from)
        .collect();
    let lines2: Vec<String> = fs::read_to_string(d2.join("Patient.csv"))
        .unwrap()
        .lines()
        .skip(1)
        .map(String::from)
        .collect();

    assert_eq!(lines1.len(), lines2.len());
    for (r1, r2) in lines1.iter().zip(lines2.iter()) {
        let c1: Vec<&str> = r1.split(',').collect();
        let c2: Vec<&str> = r2.split(',').collect();
        // v1 columns: [id, firstName, email, dateOfBirth]
        // v2 columns: [id, firstName, phone, email, dateOfBirth]
        assert_eq!(c1[0], c2[0], "id changed when phone field was inserted");
        assert_eq!(c1[1], c2[1], "firstName changed when phone field was inserted");
        assert_eq!(c1[2], c2[3], "email changed when phone field was inserted");
        assert_eq!(c1[3], c2[4], "dateOfBirth changed when phone field was inserted");
    }

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_ecommerce_and_university_schemas_validate_cleanly() {
    let root = repo_root();
    let tmp = std::env::temp_dir().join("sdg_rust_test_domains");
    let _ = fs::remove_dir_all(&tmp);

    for name in ["ecommerce", "university"] {
        let sdl = fs::read_to_string(root.join(format!("configs/schemas/{}.graphql", name))).unwrap();
        let dict = DataDictionary::built_in().unwrap();
        let compiled = CompiledSchema::compile(&sdl, dict, None, 1.0, &BTreeMap::new()).unwrap();
        let out = tmp.join(name);
        execute_schema(&compiled, 42, &out, OutputFormat::Both, 1.0, None, None).unwrap();
        let rep = validate_schema_output(&compiled, &out).unwrap();
        assert_eq!(
            rep.summary.conformance_errors, 0,
            "{} had conformance errors: {:?}",
            name, rep.errors
        );
    }

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_graphql_ontology_binding_and_unknown_concept_rejection() {
    let root = repo_root();
    let onto_build = root.join("tests/fixtures/ontology-build");

    let valid_sdl = r#"
        type NetworkAlarm @count(25) @ontology(concepts: ["noria:EventRecord", "tmf642:Alarm"]) {
            id: ID!
            severity: PerceivedSeverity!
            alarmType: AlarmType!
        }

        enum PerceivedSeverity @ontology(vocabulary: "ietf-alarms", owner: "ietf-alarms:typedef/severity") {
            PLACEHOLDER
        }

        enum AlarmType @ontology(vocabulary: "tmf642", owner: "tmf642:AlarmType") {
            PLACEHOLDER
        }
    "#;
    let dict = DataDictionary::built_in().unwrap();
    let compiled =
        CompiledSchema::compile(valid_sdl, dict, Some(&onto_build), 1.0, &BTreeMap::new()).unwrap();
    assert_eq!(
        compiled.enums["PerceivedSeverity"],
        vec!["indeterminate", "warning", "minor", "major", "critical"]
    );

    let invalid_sdl = r#"
        type NetworkAlarm @count(10) @ontology(concepts: ["noria:NonExistentConcept"]) {
            id: ID!
        }
    "#;
    let err = CompiledSchema::compile(
        invalid_sdl,
        DataDictionary::built_in().unwrap(),
        Some(&onto_build),
        1.0,
        &BTreeMap::new(),
    );
    assert!(err.is_err(), "unknown ontology concept must fail compilation");
}

#[test]
fn test_cyclic_foreign_key_schema_rejected_at_compile_time() {
    let cyclic_sdl = r#"
        type Alpha @count(10) {
            id: ID!
            beta: Beta!
        }
        type Beta @count(10) {
            id: ID!
            alpha: Alpha!
        }
    "#;
    let res = CompiledSchema::compile(
        cyclic_sdl,
        DataDictionary::built_in().unwrap(),
        None,
        1.0,
        &BTreeMap::new(),
    );
    assert!(res.is_err(), "cyclic scalar FK dependency must fail topological sort");
}

#[test]
fn test_rust_telecom_pack_generation_and_validation() {
    let root = repo_root();
    let onto_build = root.join("tests/fixtures/ontology-build");
    let raw = fs::read_to_string(root.join("configs/telecom/abilene-test.yaml")).unwrap();
    let mut cfg: TelecomConfig = serde_yaml::from_str(&raw).unwrap();
    cfg.topology = root
        .join("tests/fixtures/Abilene.graphml")
        .to_string_lossy()
        .to_string();

    let tmp = std::env::temp_dir().join("sdg_rust_test_telecom");
    let _ = fs::remove_dir_all(&tmp);

    let m = generate_telecom(&cfg, 7, &onto_build, &tmp, None).unwrap();
    assert!(m["faults"].as_u64().unwrap() > 0);
    assert!(m["alarms"].as_u64().unwrap() > 0);

    let rep = validate_telecom(&tmp).unwrap();
    assert_eq!(rep["summary"]["conformance_errors"].as_u64().unwrap(), 0);

    let _ = fs::remove_dir_all(&tmp);
}


#[test]
fn test_sql_ddl_frontend_health_clinic_end_to_end() {
    let root = repo_root();
    let sql_path = root.join("configs/schemas/health_clinic.sql");
    let dict = DataDictionary::built_in().unwrap();
    let compiled = CompiledSchema::compile_file(&sql_path, dict, None, 1.0, &BTreeMap::new()).unwrap();
    assert_eq!(compiled.entities.len(), 4);

    let tmp = std::env::temp_dir().join("sdg_rust_test_sql_frontend");
    let _ = fs::remove_dir_all(&tmp);
    let manifest =
        execute_schema(&compiled, 20261019, &tmp, OutputFormat::Both, 1.0, Some(&sql_path), None)
            .unwrap();
    assert_eq!(manifest.total_records, 50 + 200 + 500 + 250);

    let rep = validate_schema_output(&compiled, &tmp).unwrap();
    assert_eq!(
        rep.summary.conformance_errors, 0,
        "SQL DDL frontend conformance errors: {:?}",
        rep.errors
    );
    assert_eq!(rep.summary.rows_checked, 1000);
    assert_eq!(rep.summary.foreign_keys_checked, 500 * 2 + 250);
    assert_eq!(rep.summary.temporal_constraints_checked, 500 + 250);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_turtle_ontology_frontend_end_to_end() {
    let root = repo_root();
    let ttl_path = root.join("configs/schemas/clinical_ontology.ttl");
    let dict = DataDictionary::built_in().unwrap();
    let compiled = CompiledSchema::compile_file(&ttl_path, dict, None, 1.0, &BTreeMap::new()).unwrap();
    assert_eq!(compiled.entities.len(), 3);
    assert!(compiled.enums.contains_key("EncounterSeverity"));

    let tmp = std::env::temp_dir().join("sdg_rust_test_ttl_frontend");
    let _ = fs::remove_dir_all(&tmp);
    let manifest =
        execute_schema(&compiled, 20261019, &tmp, OutputFormat::Both, 1.0, Some(&ttl_path), None)
            .unwrap();
    assert_eq!(manifest.total_records, 25 + 100 + 300);

    let rep = validate_schema_output(&compiled, &tmp).unwrap();
    assert_eq!(
        rep.summary.conformance_errors, 0,
        "Turtle ontology frontend conformance errors: {:?}",
        rep.errors
    );
    assert_eq!(rep.summary.rows_checked, 425);
    assert_eq!(rep.summary.foreign_keys_checked, 300 * 2);
    assert_eq!(rep.summary.temporal_constraints_checked, 300);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_first_class_edge_and_derived_and_weighted_enums() {
    let root = repo_root();
    let gql_path = root.join("configs/schemas/banking_graph.graphql");
    let dict = DataDictionary::built_in().unwrap();
    let compiled = CompiledSchema::compile_file(&gql_path, dict, None, 1.0, &BTreeMap::new()).unwrap();

    let tmp = std::env::temp_dir().join("sdg_rust_test_banking_graph");
    let _ = fs::remove_dir_all(&tmp);
    let manifest =
        execute_schema(&compiled, 20261019, &tmp, OutputFormat::Both, 1.0, Some(&gql_path), None)
            .unwrap();
    assert_eq!(manifest.total_records, 500);
    assert_eq!(manifest.counts.relations.get("TRANSFERRED"), Some(&400));

    let rep = validate_schema_output(&compiled, &tmp).unwrap();
    assert_eq!(
        rep.summary.conformance_errors, 0,
        "banking_graph errors: {:?}",
        rep.errors
    );

    // Verify @derived(formula: "balance - holds") in Account.csv
    let acc_csv = fs::read_to_string(tmp.join("Account.csv")).unwrap();
    for line in acc_csv.lines().skip(1) {
        let cols: Vec<&str> = line.split(',').collect();
        // id, accountNumber, ownerName, balance, holds, availableBalance, riskTier
        let balance: f64 = cols[3].parse().unwrap();
        let holds: f64 = cols[4].parse().unwrap();
        let avail: f64 = cols[5].parse().unwrap();
        assert!(
            ((balance - holds) - avail).abs() < 0.02,
            "derived availableBalance mismatch: {} - {} != {}",
            balance,
            holds,
            avail
        );
    }

    // Verify first-class @edge emitted rich attributed edges in edges.jsonl and only Account nodes in nodes.jsonl
    let nodes_lines: Vec<String> = fs::read_to_string(tmp.join("nodes.jsonl"))
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(
        nodes_lines.len(),
        100,
        "Transaction @edge must emit into edges.jsonl, not nodes.jsonl"
    );

    let edges_lines: Vec<String> = fs::read_to_string(tmp.join("edges.jsonl"))
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(edges_lines.len(), 400);
    let sample_edge: serde_json::Value = serde_json::from_str(&edges_lines[0]).unwrap();
    assert_eq!(sample_edge["rel"], "TRANSFERRED");
    assert!(sample_edge.get("amount").is_some());
    assert!(sample_edge.get("fee").is_some());
    assert!(sample_edge.get("isLargeTransfer").is_some());
    assert!(sample_edge.get("channel").is_some());

    let _ = fs::remove_dir_all(&tmp);
}
