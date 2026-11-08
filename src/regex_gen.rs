//! Compile-time Regex Pattern AST and Zero-Allocation Generator.
//!
//! Parses regex patterns from the Attribute Data Dictionary or `@regex(pattern: "...")`
//! directives into a compact AST (`CompiledRegex`) and synthesizes conforming strings directly
//! into a reusable buffer using `SeekableRng`. Also retains a compiled `regex::Regex`
//! instance for fast conformance validation.

use crate::rng::SeekableRng;
use anyhow::{anyhow, Result};
use regex::Regex;

#[derive(Clone, Debug)]
enum RegexAst {
    Literal(char),
    CharClass(Vec<char>),
    Sequence(Vec<RegexAst>),
    Alternation(Vec<RegexAst>),
    Repeat {
        inner: Box<RegexAst>,
        min: u32,
        max: u32,
    },
}

#[derive(Clone, Debug)]
pub struct CompiledRegex {
    pattern: String,
    ast: RegexAst,
    validator: Regex,
}

impl CompiledRegex {
    pub fn compile(pattern: &str) -> Result<Self> {
        let trimmed = pattern
            .trim()
            .strip_prefix('^')
            .unwrap_or(pattern.trim());
        let trimmed = trimmed.strip_suffix('$').unwrap_or(trimmed);
        let chars: Vec<char> = trimmed.chars().collect();
        let mut pos = 0;
        let ast = parse_alternation(&chars, &mut pos)?;
        if pos != chars.len() {
            return Err(anyhow!(
                "unexpected trailing regex token at position {} in {:?}",
                pos,
                pattern
            ));
        }
        let anchored = format!("^(?:{})$", trimmed);
        let validator = Regex::new(&anchored)
            .map_err(|e| anyhow!("invalid regex pattern {:?}: {}", pattern, e))?;
        Ok(Self {
            pattern: pattern.to_string(),
            ast,
            validator,
        })
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    pub fn is_match(&self, value: &str) -> bool {
        self.validator.is_match(value)
    }

    #[inline]
    pub fn generate_into(&self, rng: &mut SeekableRng, out: &mut String) {
        eval_ast(&self.ast, rng, out);
    }

    pub fn generate(&self, rng: &mut SeekableRng) -> String {
        let mut out = String::with_capacity(32);
        self.generate_into(rng, &mut out);
        out
    }
}

fn eval_ast(node: &RegexAst, rng: &mut SeekableRng, out: &mut String) {
    match node {
        RegexAst::Literal(c) => out.push(*c),
        RegexAst::CharClass(pool) => {
            if !pool.is_empty() {
                out.push(*rng.choose(pool));
            }
        }
        RegexAst::Sequence(items) => {
            for item in items {
                eval_ast(item, rng, out);
            }
        }
        RegexAst::Alternation(branches) => {
            if !branches.is_empty() {
                let chosen = rng.choose(branches);
                eval_ast(chosen, rng, out);
            }
        }
        RegexAst::Repeat { inner, min, max } => {
            let count = if min == max {
                *min
            } else {
                rng.range_i64(*min as i64, *max as i64) as u32
            };
            for _ in 0..count {
                eval_ast(inner, rng, out);
            }
        }
    }
}

fn parse_alternation(chars: &[char], pos: &mut usize) -> Result<RegexAst> {
    let mut branches = vec![parse_sequence(chars, pos)?];
    while *pos < chars.len() && chars[*pos] == '|' {
        *pos += 1;
        branches.push(parse_sequence(chars, pos)?);
    }
    if branches.len() == 1 {
        Ok(branches.pop().unwrap())
    } else {
        Ok(RegexAst::Alternation(branches))
    }
}

fn parse_sequence(chars: &[char], pos: &mut usize) -> Result<RegexAst> {
    let mut seq = Vec::new();
    while *pos < chars.len() && chars[*pos] != ')' && chars[*pos] != '|' {
        let atom = parse_atom(chars, pos)?;
        let quantified = parse_quantifier(chars, pos, atom)?;
        seq.push(quantified);
    }
    if seq.len() == 1 {
        Ok(seq.pop().unwrap())
    } else {
        Ok(RegexAst::Sequence(seq))
    }
}

fn parse_atom(chars: &[char], pos: &mut usize) -> Result<RegexAst> {
    if *pos >= chars.len() {
        return Err(anyhow!("unexpected end of regex"));
    }
    let c = chars[*pos];
    match c {
        '(' => {
            *pos += 1;
            // Support non-capturing `(?:...)`
            if *pos + 1 < chars.len() && chars[*pos] == '?' && chars[*pos + 1] == ':' {
                *pos += 2;
            }
            let inner = parse_alternation(chars, pos)?;
            if *pos >= chars.len() || chars[*pos] != ')' {
                return Err(anyhow!("unclosed '(' in regex"));
            }
            *pos += 1;
            Ok(inner)
        }
        '[' => {
            *pos += 1;
            let mut pool = Vec::new();
            while *pos < chars.len() && chars[*pos] != ']' {
                let start = if chars[*pos] == '\\' {
                    *pos += 1;
                    if *pos >= chars.len() {
                        return Err(anyhow!("dangling escape in char class"));
                    }
                    let esc = chars[*pos];
                    *pos += 1;
                    match esc {
                        'd' => {
                            pool.extend('0'..='9');
                            continue;
                        }
                        'w' => {
                            pool.extend('a'..='z');
                            pool.extend('A'..='Z');
                            pool.extend('0'..='9');
                            pool.push('_');
                            continue;
                        }
                        's' => {
                            pool.push(' ');
                            continue;
                        }
                        other => other,
                    }
                } else {
                    let ch = chars[*pos];
                    *pos += 1;
                    ch
                };

                if *pos + 1 < chars.len() && chars[*pos] == '-' && chars[*pos + 1] != ']' {
                    *pos += 1; // skip '-'
                    let end = chars[*pos];
                    *pos += 1;
                    if start <= end {
                        pool.extend(start..=end);
                    } else {
                        pool.push(start);
                        pool.push(end);
                    }
                } else {
                    pool.push(start);
                }
            }
            if *pos >= chars.len() || chars[*pos] != ']' {
                return Err(anyhow!("unclosed '[' in regex"));
            }
            *pos += 1;
            Ok(RegexAst::CharClass(pool))
        }
        '\\' => {
            *pos += 1;
            if *pos >= chars.len() {
                return Err(anyhow!("trailing '\\' in regex"));
            }
            let esc = chars[*pos];
            *pos += 1;
            match esc {
                'd' => Ok(RegexAst::CharClass(('0'..='9').collect())),
                'w' => {
                    let mut p: Vec<char> = ('a'..='z').collect();
                    p.extend('A'..='Z');
                    p.extend('0'..='9');
                    p.push('_');
                    Ok(RegexAst::CharClass(p))
                }
                's' => Ok(RegexAst::Literal(' ')),
                other => Ok(RegexAst::Literal(other)),
            }
        }
        '.' => {
            *pos += 1;
            let mut p: Vec<char> = ('a'..='z').collect();
            p.extend('0'..='9');
            Ok(RegexAst::CharClass(p))
        }
        other => {
            *pos += 1;
            Ok(RegexAst::Literal(other))
        }
    }
}

fn parse_quantifier(chars: &[char], pos: &mut usize, atom: RegexAst) -> Result<RegexAst> {
    if *pos >= chars.len() {
        return Ok(atom);
    }
    match chars[*pos] {
        '?' => {
            *pos += 1;
            Ok(RegexAst::Repeat {
                inner: Box::new(atom),
                min: 0,
                max: 1,
            })
        }
        '+' => {
            *pos += 1;
            Ok(RegexAst::Repeat {
                inner: Box::new(atom),
                min: 1,
                max: 6,
            })
        }
        '*' => {
            *pos += 1;
            Ok(RegexAst::Repeat {
                inner: Box::new(atom),
                min: 0,
                max: 5,
            })
        }
        '{' => {
            *pos += 1;
            let start = *pos;
            while *pos < chars.len() && chars[*pos] != '}' {
                *pos += 1;
            }
            if *pos >= chars.len() {
                return Err(anyhow!("unclosed '{{' in regex quantifier"));
            }
            let spec: String = chars[start..*pos].iter().collect();
            *pos += 1; // skip '}'
            let (min, max) = if let Some((lo, hi)) = spec.split_once(',') {
                let lo_n: u32 = lo.trim().parse()?;
                let hi_n: u32 = if hi.trim().is_empty() {
                    lo_n + 5
                } else {
                    hi.trim().parse()?
                };
                (lo_n, hi_n.max(lo_n))
            } else {
                let n: u32 = spec.trim().parse()?;
                (n, n)
            };
            Ok(RegexAst::Repeat {
                inner: Box::new(atom),
                min,
                max,
            })
        }
        _ => Ok(atom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compiled_regex_email_and_icd10_and_phone() {
        let patterns = [
            r"^[a-z]{5,10}\.[a-z]{4,8}@(clinic|hospital|example)\.(com|org)$",
            r"^[A-TV-Z][0-9]{2}\.[0-9]{1,2}$",
            r"^\+1-[2-9][0-9]{2}-[0-9]{3}-[0-9]{4}$",
            r"^SKU-[A-Z]{3}-[0-9]{5}$",
        ];
        let mut rng = SeekableRng::for_stage(42, "regex_test");
        for pat in &patterns {
            let compiled = CompiledRegex::compile(pat).expect("valid regex");
            for _ in 0..200 {
                let s = compiled.generate(&mut rng);
                assert!(
                    compiled.is_match(&s),
                    "generated string {:?} did not match pattern {:?}",
                    s,
                    pat
                );
            }
        }
    }
}
