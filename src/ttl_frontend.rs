//! OWL / RDFS / SKOS Turtle (`.ttl`) Ontology Frontend for `CompiledSchema`.
//!
//! Compiles `.ttl` ontology files directly into the shared schema AST (`RawEntity`, `RawField`,
//! `RawEnum`, `RawScalar`), mapping:
//! - `owl:Class` / `rdfs:Class` -> `RawEntity`
//! - `owl:ObjectProperty` (`rdfs:domain` -> `rdfs:range`) -> Foreign-Key `RawField`
//! - `owl:DatatypeProperty` (`rdfs:domain` -> `rdfs:range` `xsd:*`) -> Scalar `RawField`
//! - `skos:ConceptScheme` + `skos:inScheme` individuals -> `RawEnum`
//! - `rdfs:subClassOf` -> single-inheritance property propagation from superclass to subclass
//! - `# @count(N)`, `# @pk(...)`, `# @fk(...)`, `# @after(...)`, `# @regex(...)`, `# @dist(...)`,
//!   `# @derived(...)`, `# @edge(...)` comment directives on classes and properties.

use crate::schema::{
    parse_directives_from_str, ParsedDirective, RawEntity, RawEnum, RawField, RawScalar,
};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default)]
struct TtlSubjectBlock {
    subject: String,
    directives: Vec<ParsedDirective>,
    pred_objs: Vec<(String, Vec<String>)>,
}

pub fn parse_turtle_ontology(ttl: &str) -> Result<(Vec<RawEntity>, Vec<RawEnum>, Vec<RawScalar>)> {
    let blocks = parse_ttl_blocks(ttl)?;

    let mut classes: BTreeMap<String, Vec<ParsedDirective>> = BTreeMap::new();
    let mut subclass_of: BTreeMap<String, String> = BTreeMap::new();
    let mut obj_props: Vec<(String, String, String, Vec<ParsedDirective>)> = Vec::new();
    let mut data_props: Vec<(String, String, String, Vec<ParsedDirective>)> = Vec::new();
    let mut schemes: BTreeSet<String> = BTreeSet::new();
    let mut scheme_members: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for b in &blocks {
        let subj_local = local_name(&b.subject);
        if subj_local.is_empty() || subj_local.starts_with('_') {
            continue;
        }

        let mut rdf_types = Vec::new();
        let mut domains = Vec::new();
        let mut ranges = Vec::new();
        let mut in_schemes = Vec::new();

        for (pred, objs) in &b.pred_objs {
            let p_local = if pred == "a" {
                "type".to_string()
            } else {
                local_name(pred)
            };
            match p_local.as_str() {
                "type" => {
                    for o in objs {
                        rdf_types.push(local_name(o));
                    }
                }
                "subClassOf" => {
                    if let Some(parent) = objs.first() {
                        let p_name = local_name(parent);
                        if !p_name.is_empty() {
                            subclass_of.insert(subj_local.clone(), p_name);
                        }
                    }
                }
                "domain" => {
                    for o in objs {
                        let d = local_name(o);
                        if !d.is_empty() {
                            domains.push(d);
                        }
                    }
                }
                "range" => {
                    for o in objs {
                        let r = local_name(o);
                        if !r.is_empty() {
                            ranges.push(r);
                        }
                    }
                }
                "inScheme" => {
                    for o in objs {
                        let s = local_name(o);
                        if !s.is_empty() {
                            in_schemes.push(s);
                        }
                    }
                }
                _ => {}
            }
        }

        if rdf_types.iter().any(|t| t == "Class") {
            classes
                .entry(subj_local.clone())
                .or_default()
                .extend(b.directives.clone());
        }
        if rdf_types.iter().any(|t| t == "ConceptScheme") {
            schemes.insert(subj_local.clone());
        }
        for s in in_schemes {
            schemes.insert(s.clone());
            scheme_members
                .entry(s)
                .or_default()
                .push(subj_local.clone());
        }

        if rdf_types.iter().any(|t| t == "ObjectProperty") {
            if let (Some(dom), Some(rng)) = (domains.first(), ranges.first()) {
                classes.entry(dom.clone()).or_default();
                obj_props.push((
                    subj_local.clone(),
                    dom.clone(),
                    rng.clone(),
                    b.directives.clone(),
                ));
            }
        } else if rdf_types.iter().any(|t| t == "DatatypeProperty") {
            if let Some(dom) = domains.first() {
                let rng = ranges
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "string".to_string());
                classes.entry(dom.clone()).or_default();
                data_props.push((subj_local.clone(), dom.clone(), rng, b.directives.clone()));
            }
        }
    }

    // Build RawEnums from SKOS ConceptSchemes
    let mut enums = Vec::new();
    let mut enum_names = BTreeSet::new();
    for (scheme, values) in scheme_members {
        if !values.is_empty() {
            enum_names.insert(scheme.clone());
            enums.push(RawEnum {
                name: scheme,
                directives: Vec::new(),
                values,
            });
        }
    }

    // Assemble fields per class
    let mut fields_by_class: BTreeMap<String, Vec<RawField>> = BTreeMap::new();
    for cls in classes.keys() {
        fields_by_class.insert(
            cls.clone(),
            vec![RawField {
                name: "id".to_string(),
                base_type: "ID".to_string(),
                is_list: false,
                non_null: true,
                directives: vec![ParsedDirective {
                    name: "pk".to_string(),
                    ..Default::default()
                }],
            }],
        );
    }

    // Attach DatatypeProperties
    for (prop_name, dom, rng, dirs) in &data_props {
        let scalar = map_xsd_to_scalar(rng, prop_name);
        if let Some(list) = fields_by_class.get_mut(dom) {
            if !list.iter().any(|f| f.name == *prop_name) {
                list.push(RawField {
                    name: prop_name.clone(),
                    base_type: scalar,
                    is_list: false,
                    non_null: true,
                    directives: dirs.clone(),
                });
            }
        }
    }

    // Attach ObjectProperties (pointing either to another Class or a SKOS ConceptScheme Enum)
    for (prop_name, dom, rng, dirs) in &obj_props {
        let is_known_target = classes.contains_key(rng) || enum_names.contains(rng);
        let target_type = if is_known_target {
            rng.clone()
        } else {
            "String".to_string()
        };
        if let Some(list) = fields_by_class.get_mut(dom) {
            if !list.iter().any(|f| f.name == *prop_name) {
                list.push(RawField {
                    name: prop_name.clone(),
                    base_type: target_type,
                    is_list: false,
                    non_null: true,
                    directives: dirs.clone(),
                });
            }
        }
    }

    // Propagate inherited fields along `rdfs:subClassOf` (1-hop and 2-hop)
    for _ in 0..2 {
        let snapshot = fields_by_class.clone();
        for (child, parent) in &subclass_of {
            if let Some(parent_fields) = snapshot.get(parent) {
                if let Some(child_fields) = fields_by_class.get_mut(child) {
                    for pf in parent_fields {
                        if pf.name != "id" && !child_fields.iter().any(|cf| cf.name == pf.name) {
                            child_fields.push(pf.clone());
                        }
                    }
                }
            }
        }
    }

    // Ensure every class has at least one descriptive attribute (`label: String!`) if none declared
    let mut entities = Vec::new();
    for (cls_name, dirs) in classes {
        let count = dirs
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

        let mut fields = fields_by_class.remove(&cls_name).unwrap_or_default();
        if fields.len() <= 1 {
            fields.push(RawField {
                name: "name".to_string(),
                base_type: "String".to_string(),
                is_list: false,
                non_null: true,
                directives: Vec::new(),
            });
        }

        entities.push(RawEntity {
            name: cls_name,
            count,
            directives: dirs,
            fields,
        });
    }

    Ok((entities, enums, Vec::new()))
}

fn map_xsd_to_scalar(xsd_range: &str, prop_name: &str) -> String {
    let lower = xsd_range.to_lowercase();
    let prop_lower = prop_name.to_lowercase();
    if prop_lower.contains("email") {
        return "Email".to_string();
    }
    match lower.as_str() {
        "integer" | "int" | "long" | "short" | "nonnegativeinteger" | "positiveinteger" => {
            "Int".to_string()
        }
        "decimal" | "double" | "float" => "Float".to_string(),
        "boolean" | "bool" => "Boolean".to_string(),
        "date" | "datetime" | "datetimestamp" => "Date".to_string(),
        "time" => "Time".to_string(),
        _ => "String".to_string(),
    }
}

fn local_name(term: &str) -> String {
    let trimmed = term.trim().trim_matches(|c| c == '<' || c == '>');
    if let Some(idx) = trimmed.rfind('#') {
        return trimmed[idx + 1..].to_string();
    }
    if trimmed.contains("://") {
        if let Some(idx) = trimmed.rfind('/') {
            return trimmed[idx + 1..].to_string();
        }
    }
    if let Some((_, local)) = trimmed.split_once(':') {
        return local.to_string();
    }
    trimmed.to_string()
}

fn parse_ttl_blocks(ttl: &str) -> Result<Vec<TtlSubjectBlock>> {
    let mut blocks = Vec::new();
    let mut pending_directives: Vec<ParsedDirective> = Vec::new();
    let mut current_stmt = String::new();
    let mut stmt_directives: Vec<ParsedDirective> = Vec::new();

    for raw_line in ttl.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(comment) = trimmed.strip_prefix('#') {
            let dirs = parse_directives_from_str(comment)?;
            if current_stmt.trim().is_empty() {
                pending_directives.extend(dirs);
            } else {
                stmt_directives.extend(dirs);
            }
            continue;
        }
        if trimmed.starts_with("@prefix")
            || trimmed.starts_with("@base")
            || trimmed.to_uppercase().starts_with("PREFIX ")
            || trimmed.to_uppercase().starts_with("BASE ")
        {
            pending_directives.clear();
            continue;
        }

        // Strip trailing comment while preserving `#` inside `<http://...#fragment>` or `"..."`
        let (code_part, inline_comment) = split_ttl_line_comment(trimmed);
        if let Some(cm) = inline_comment {
            let dirs = parse_directives_from_str(cm)?;
            if current_stmt.trim().is_empty() {
                pending_directives.extend(dirs);
            } else {
                stmt_directives.extend(dirs);
            }
        }

        if current_stmt.trim().is_empty() {
            stmt_directives.splice(0..0, std::mem::take(&mut pending_directives));
        }
        if !current_stmt.is_empty() {
            current_stmt.push(' ');
        }
        current_stmt.push_str(code_part.trim());

        if statement_ends_with_dot(&current_stmt) {
            if let Some(block) =
                parse_single_ttl_block(&current_stmt, std::mem::take(&mut stmt_directives))
            {
                blocks.push(block);
            }
            current_stmt.clear();
        }
    }

    Ok(blocks)
}

fn split_ttl_line_comment(line: &str) -> (&str, Option<&str>) {
    let mut in_quotes = false;
    let mut in_iri = false;
    let mut prev_esc = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '"' if !prev_esc && !in_iri => in_quotes = !in_quotes,
            '<' if !in_quotes => in_iri = true,
            '>' if !in_quotes && in_iri => in_iri = false,
            '#' if !in_quotes && !in_iri => return (&line[..idx], Some(&line[idx + 1..])),
            _ => {}
        }
        prev_esc = ch == '\\' && !prev_esc;
    }
    (line, None)
}

fn statement_ends_with_dot(stmt: &str) -> bool {
    let t = stmt.trim_end();
    if !t.ends_with('.') {
        return false;
    }
    // Make sure it's not a float literal or inside `<...>`
    let without_dot = t[..t.len() - 1].trim_end();
    !without_dot.ends_with('<')
}

fn parse_single_ttl_block(stmt: &str, directives: Vec<ParsedDirective>) -> Option<TtlSubjectBlock> {
    let body = stmt.trim().strip_suffix('.')?.trim();
    let tokens = tokenize_ttl(body);
    if tokens.len() < 3 {
        return None;
    }
    let subject = tokens[0].clone();
    let mut pred_objs = Vec::new();
    let mut idx = 1;
    while idx < tokens.len() {
        let pred = tokens[idx].clone();
        idx += 1;
        let mut objs = Vec::new();
        while idx < tokens.len() {
            let tok = &tokens[idx];
            if tok == ";" {
                idx += 1;
                break;
            } else if tok == "," {
                idx += 1;
                continue;
            } else {
                objs.push(tok.clone());
                idx += 1;
            }
        }
        if !objs.is_empty() {
            pred_objs.push((pred, objs));
        }
    }

    Some(TtlSubjectBlock {
        subject,
        directives,
        pred_objs,
    })
}

fn tokenize_ttl(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut bracket_depth = 0i32;

    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        // Skip blank node property lists `[ ... ]`
        if chars[i] == '[' {
            bracket_depth += 1;
            i += 1;
            while i < chars.len() && bracket_depth > 0 {
                if chars[i] == '[' {
                    bracket_depth += 1;
                } else if chars[i] == ']' {
                    bracket_depth -= 1;
                }
                i += 1;
            }
            out.push("_:bnode".to_string());
            continue;
        }
        if chars[i] == ';' || chars[i] == ',' {
            out.push(chars[i].to_string());
            i += 1;
            continue;
        }
        if chars[i] == '<' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '>' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
            continue;
        }
        if chars[i] == '"' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < chars.len() {
                i += 1;
            }
            // Skip optional `@lang` or `^^xsd:type`
            while i < chars.len()
                && !chars[i].is_whitespace()
                && chars[i] != ';'
                && chars[i] != ','
            {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
            continue;
        }
        let start = i;
        while i < chars.len()
            && !chars[i].is_whitespace()
            && chars[i] != ';'
            && chars[i] != ','
        {
            i += 1;
        }
        out.push(chars[start..i].iter().collect());
    }
    out
}
