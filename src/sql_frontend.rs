//! SQL DDL (`CREATE TABLE` / `CREATE TYPE AS ENUM`) Frontend for `CompiledSchema`.
//!
//! Compiles standard SQL DDL files (`.sql`) into the shared raw schema AST (`RawEntity`,
//! `RawField`, `RawEnum`, `RawScalar`), supporting:
//! - `CREATE TYPE <Name> AS ENUM ('A', 'B', ...);`
//! - `CREATE TABLE [IF NOT EXISTS] <Name> ( ... );`
//! - Inline `PRIMARY KEY`, `NOT NULL`, `REFERENCES <Target>(<col>)`
//! - Table-level `PRIMARY KEY (<col>)` and `FOREIGN KEY (<col>) REFERENCES <Target>(<col>)`
//! - `CHECK (<col> IN ('A', 'B', ...))` and `CHECK (<col> BETWEEN <min> AND <max>)` / `>= ... <=`
//! - SQL comment directives (`-- @count(1000)`, `-- @fk(dist: "zipf", s: 1.2)`,
//!   `-- @after(field: "...")`, `-- @regex(pattern: "...")`, `-- @derived(formula: "...")`,
//!   `-- @edge(from: "...", to: "...", type: "...")`)

use crate::schema::{
    parse_directives_from_str, ParsedDirective, RawEntity, RawEnum, RawField, RawScalar,
};
use anyhow::{bail, Result};

pub fn parse_sql_ddl(sql: &str) -> Result<(Vec<RawEntity>, Vec<RawEnum>, Vec<RawScalar>)> {
    let mut entities = Vec::new();
    let mut enums = Vec::new();
    let scalars = Vec::new();

    let mut pending_directives: Vec<ParsedDirective> = Vec::new();
    let lines: Vec<&str> = sql.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let raw_line = lines[i].trim();
        if raw_line.is_empty() {
            i += 1;
            continue;
        }

        // Check if this is a standalone comment line
        if let Some(comment) = raw_line.strip_prefix("--") {
            let dirs = parse_directives_from_str(comment)?;
            pending_directives.extend(dirs);
            i += 1;
            continue;
        }

        let upper = raw_line.to_uppercase();
        if upper.starts_with("CREATE TYPE ") {
            // Collect until ';'
            let mut stmt = String::new();
            while i < lines.len() {
                let l = lines[i];
                let code_part = l.split("--").next().unwrap_or(l);
                stmt.push_str(code_part);
                stmt.push(' ');
                i += 1;
                if code_part.contains(';') {
                    break;
                }
            }
            if let Some(en) = parse_create_type_enum(&stmt, std::mem::take(&mut pending_directives))?
            {
                enums.push(en);
            }
            continue;
        }

        if upper.starts_with("CREATE TABLE ") {
            // Extract any inline directives on the CREATE TABLE header line
            if let Some((_, comment)) = raw_line.split_once("--") {
                pending_directives.extend(parse_directives_from_str(comment)?);
            }
            let header_code = raw_line.split("--").next().unwrap_or(raw_line).trim();
            let table_name = extract_table_name(header_code)?;

            // Collect lines until the closing `);` of CREATE TABLE
            let mut body_lines: Vec<String> = Vec::new();
            // If header line already has '(' and content after '('
            if let Some(pos) = header_code.find('(') {
                let after = header_code[pos + 1..].trim();
                if !after.is_empty() && !after.starts_with(')') {
                    body_lines.push(after.to_string());
                }
            }
            i += 1;
            let mut depth = header_code.chars().filter(|&c| c == '(').count() as i32
                - header_code.chars().filter(|&c| c == ')').count() as i32;

            while i < lines.len() && depth > 0 {
                let line = lines[i];
                let code_only = line.split("--").next().unwrap_or(line);
                for ch in code_only.chars() {
                    if ch == '(' {
                        depth += 1;
                    } else if ch == ')' {
                        depth -= 1;
                    }
                }
                body_lines.push(line.to_string());
                i += 1;
            }

            let ent = parse_create_table_body(
                table_name,
                &body_lines,
                std::mem::take(&mut pending_directives),
            )?;
            entities.push(ent);
            continue;
        }

        pending_directives.clear();
        i += 1;
    }

    Ok((entities, enums, scalars))
}

fn parse_create_type_enum(
    stmt: &str,
    directives: Vec<ParsedDirective>,
) -> Result<Option<RawEnum>> {
    let upper = stmt.to_uppercase();
    let Some(as_pos) = upper.find(" AS ENUM") else {
        return Ok(None);
    };
    let prefix = stmt[..as_pos].trim();
    let name = prefix
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim_matches(|c| c == '"' || c == '`')
        .to_string();
    let Some(open_p) = stmt[as_pos..].find('(') else {
        bail!("expected '(' in CREATE TYPE {}", name);
    };
    let Some(close_p) = stmt[as_pos + open_p..].rfind(')') else {
        bail!("expected ')' in CREATE TYPE {}", name);
    };
    let inner = &stmt[as_pos + open_p + 1..as_pos + open_p + close_p];
    let values: Vec<String> = inner
        .split(',')
        .map(|s| s.trim().trim_matches('\'').trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Ok(Some(RawEnum {
        name,
        directives,
        values,
    }))
}

fn extract_table_name(header: &str) -> Result<String> {
    let before_paren = header.split('(').next().unwrap_or(header).trim();
    let tokens: Vec<&str> = before_paren.split_whitespace().collect();
    let raw_name = tokens
        .last()
        .ok_or_else(|| anyhow::anyhow!("missing table name in {:?}", header))?;
    // Strip schema qualifier e.g. public.patient -> patient
    let clean = raw_name
        .rsplit('.')
        .next()
        .unwrap_or(raw_name)
        .trim_matches(|c| c == '"' || c == '`');
    Ok(clean.to_string())
}

fn parse_create_table_body(
    table_name: String,
    body_lines: &[String],
    directives: Vec<ParsedDirective>,
) -> Result<RawEntity> {
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

    // Group lines into logical column/constraint items while preserving inline `-- @...` directives
    let mut items: Vec<(String, Vec<ParsedDirective>)> = Vec::new();
    let mut current_code = String::new();
    let mut current_dirs: Vec<ParsedDirective> = Vec::new();
    let mut paren_depth = 0i32;

    for raw_line in body_lines {
        let (code_part, comment_part) = match raw_line.split_once("--") {
            Some((c, cm)) => (c, Some(cm)),
            None => (raw_line.as_str(), None),
        };
        if let Some(cm) = comment_part {
            let dirs = parse_directives_from_str(cm)?;
            current_dirs.extend(dirs);
        }

        let trimmed = code_part.trim();
        if trimmed.is_empty() || trimmed == ");" || trimmed == ")" {
            continue;
        }
        // Strip trailing `);` if this is the final line
        let cleaned_line = trimmed
            .strip_suffix(");")
            .or_else(|| trimmed.strip_suffix(')'))
            .unwrap_or(trimmed);

        for ch in cleaned_line.chars() {
            if ch == '(' {
                paren_depth += 1;
            } else if ch == ')' {
                paren_depth -= 1;
            }
        }

        if paren_depth <= 0 && cleaned_line.trim_end().ends_with(',') {
            let without_comma = cleaned_line.trim_end().trim_end_matches(',').trim();
            if !current_code.is_empty() {
                current_code.push(' ');
            }
            current_code.push_str(without_comma);
            items.push((
                std::mem::take(&mut current_code),
                std::mem::take(&mut current_dirs),
            ));
            paren_depth = 0;
        } else {
            if !current_code.is_empty() {
                current_code.push(' ');
            }
            current_code.push_str(cleaned_line.trim());
        }
    }
    if !current_code.trim().is_empty() {
        items.push((current_code.trim().to_string(), current_dirs));
    }

    let mut fields: Vec<RawField> = Vec::new();
    let mut table_fks: Vec<(String, String, Vec<ParsedDirective>)> = Vec::new();
    let mut table_pks: Vec<String> = Vec::new();

    for (code, mut item_dirs) in items {
        let trimmed = code.trim();
        if trimmed.is_empty() {
            continue;
        }
        let upper = trimmed.to_uppercase();

        // Table-level FOREIGN KEY (col) REFERENCES Target(id)
        if upper.starts_with("FOREIGN KEY") || upper.starts_with("CONSTRAINT ") {
            if let Some((col_name, target_table)) = parse_table_level_fk(trimmed) {
                table_fks.push((col_name, target_table, item_dirs));
                continue;
            }
            if upper.contains("PRIMARY KEY") {
                if let Some(pk_col) = extract_paren_ident(trimmed) {
                    table_pks.push(pk_col);
                }
                continue;
            }
            continue;
        }

        if upper.starts_with("PRIMARY KEY") {
            if let Some(pk_col) = extract_paren_ident(trimmed) {
                table_pks.push(pk_col);
            }
            continue;
        }

        if upper.starts_with("UNIQUE") || upper.starts_with("CHECK") {
            continue;
        }

        // Column definition: <col_name> <SQL_TYPE> [constraints...]
        let mut tokens = trimmed.split_whitespace();
        let Some(raw_col_name) = tokens.next() else {
            continue;
        };
        let col_name = raw_col_name
            .trim_matches(|c| c == '"' || c == '`')
            .to_string();
        let Some(raw_sql_type) = tokens.next() else {
            continue;
        };

        let is_pk = upper.contains("PRIMARY KEY");
        let non_null = upper.contains("NOT NULL") || is_pk;

        // Check inline REFERENCES Target(id)
        let mut base_type = if let Some(target) = parse_inline_references(trimmed) {
            target
        } else if is_pk {
            "ID".to_string()
        } else {
            map_sql_type_to_scalar(raw_sql_type, &upper)
        };

        if is_pk && !item_dirs.iter().any(|d| d.name == "pk") {
            item_dirs.push(ParsedDirective {
                name: "pk".to_string(),
                ..Default::default()
            });
        }

        // Check inline CHECK (col IN ('A', 'B', ...)) or CHECK (col BETWEEN a AND b)
        if let Some(check_dir) = parse_inline_check_constraint(trimmed) {
            if !item_dirs.iter().any(|d| d.name == check_dir.name) {
                item_dirs.push(check_dir);
            }
        }

        // If base_type was mapped from an unknown identifier (e.g., custom enum type in SQL),
        // preserve its original casing from raw_sql_type.
        if base_type == "__CUSTOM__" {
            base_type = raw_sql_type
                .split('(')
                .next()
                .unwrap_or(raw_sql_type)
                .trim_matches(|c| c == '"' || c == '`')
                .to_string();
        }

        fields.push(RawField {
            name: col_name,
            base_type,
            is_list: false,
            non_null,
            directives: item_dirs,
        });
    }

    // Apply table-level PRIMARY KEYs
    for pk_col in table_pks {
        if let Some(f) = fields
            .iter_mut()
            .find(|f| f.name.eq_ignore_ascii_case(&pk_col))
        {
            f.base_type = "ID".to_string();
            if !f.directives.iter().any(|d| d.name == "pk") {
                f.directives.push(ParsedDirective {
                    name: "pk".to_string(),
                    ..Default::default()
                });
            }
        }
    }

    // Apply table-level FOREIGN KEYs
    for (col_name, target_table, fk_dirs) in table_fks {
        if let Some(f) = fields
            .iter_mut()
            .find(|f| f.name.eq_ignore_ascii_case(&col_name))
        {
            f.base_type = target_table;
            f.directives.extend(fk_dirs);
        }
    }

    Ok(RawEntity {
        name: table_name,
        count,
        directives,
        fields,
    })
}

fn map_sql_type_to_scalar(raw_sql_type: &str, full_line_upper: &str) -> String {
    let base = raw_sql_type
        .split('(')
        .next()
        .unwrap_or(raw_sql_type)
        .to_uppercase();
    match base.as_str() {
        "ID" | "UUID" | "SERIAL" | "BIGSERIAL" => "ID".to_string(),
        "INT" | "INTEGER" | "SMALLINT" | "BIGINT" | "TINYINT" => "Int".to_string(),
        "FLOAT" | "REAL" | "DOUBLE" | "DECIMAL" | "NUMERIC" => "Float".to_string(),
        "BOOL" | "BOOLEAN" => "Boolean".to_string(),
        "DATE" => "Date".to_string(),
        "TIME" | "TIMETZ" => "Time".to_string(),
        "TIMESTAMP" | "TIMESTAMPTZ" | "DATETIME" => "Date".to_string(),
        "VARCHAR" | "CHAR" | "CHARACTER" | "TEXT" | "STRING" | "JSON" | "JSONB" => {
            if full_line_upper.contains("EMAIL") {
                "Email".to_string()
            } else {
                "String".to_string()
            }
        }
        _ => "__CUSTOM__".to_string(),
    }
}

fn parse_inline_references(col_def: &str) -> Option<String> {
    let upper = col_def.to_uppercase();
    let idx = upper.find("REFERENCES ")?;
    let after = col_def[idx + "REFERENCES ".len()..].trim();
    let target = after
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()?
        .trim_matches(|c| c == '"' || c == '`');
    if target.is_empty() {
        None
    } else {
        Some(target.to_string())
    }
}

fn parse_table_level_fk(line: &str) -> Option<(String, String)> {
    let upper = line.to_uppercase();
    let fk_pos = upper.find("FOREIGN KEY")?;
    let after_fk = &line[fk_pos + "FOREIGN KEY".len()..];
    let col = extract_paren_ident(after_fk)?;
    let target = parse_inline_references(after_fk)?;
    Some((col, target))
}

fn extract_paren_ident(s: &str) -> Option<String> {
    let open = s.find('(')?;
    let close = s[open + 1..].find(')')?;
    let inner = s[open + 1..open + 1 + close]
        .split(',')
        .next()?
        .trim()
        .trim_matches(|c| c == '"' || c == '`');
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

fn parse_inline_check_constraint(col_def: &str) -> Option<ParsedDirective> {
    let upper = col_def.to_uppercase();
    let check_pos = upper.find("CHECK")?;
    let after = &col_def[check_pos + "CHECK".len()..];
    let open = after.find('(')?;
    let close = after.rfind(')')?;
    let expr = after[open + 1..close].trim();
    let expr_upper = expr.to_uppercase();

    // Case 1: CHECK (status IN ('A', 'B', 'C'))
    if let Some(in_pos) = expr_upper.find(" IN ") {
        let in_part = &expr[in_pos + 4..];
        let p_open = in_part.find('(')?;
        let p_close = in_part.rfind(')')?;
        let vals: Vec<String> = in_part[p_open + 1..p_close]
            .split(',')
            .map(|v| v.trim().trim_matches('\'').trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
            .collect();
        if !vals.is_empty() {
            let mut dir = ParsedDirective {
                name: "values".to_string(),
                ..Default::default()
            };
            dir.list_args.insert("items".to_string(), vals);
            return Some(dir);
        }
    }

    // Case 2: CHECK (col BETWEEN min AND max)
    if let Some(bw_pos) = expr_upper.find(" BETWEEN ") {
        let rest = &expr[bw_pos + " BETWEEN ".len()..];
        let rest_upper = rest.to_uppercase();
        if let Some(and_pos) = rest_upper.find(" AND ") {
            let min_s = rest[..and_pos].trim();
            let max_s = rest[and_pos + 5..].trim();
            if min_s.parse::<f64>().is_ok() && max_s.parse::<f64>().is_ok() {
                let mut dir = ParsedDirective {
                    name: "dist".to_string(),
                    ..Default::default()
                };
                dir.args.insert("kind".to_string(), "uniform".to_string());
                dir.args.insert("min".to_string(), min_s.to_string());
                dir.args.insert("max".to_string(), max_s.to_string());
                return Some(dir);
            }
        }
    }

    None
}
