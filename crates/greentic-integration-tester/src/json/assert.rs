use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
pub enum JsonPathOp {
    Equals,
    Contains,
    Exists,
    NotExists,
    Matches,
}

impl JsonPathOp {
    pub fn parse(input: &str) -> Result<Self> {
        match input {
            "equals" => Ok(Self::Equals),
            "contains" => Ok(Self::Contains),
            "exists" => Ok(Self::Exists),
            "not_exists" => Ok(Self::NotExists),
            "matches" => Ok(Self::Matches),
            _ => anyhow::bail!("unknown jsonpath op '{input}'"),
        }
    }
}

#[derive(Debug, Clone)]
enum PathSegment {
    Key(String),
    Index(usize),
    IndexWildcard,
}

pub fn evaluate_jsonpath(
    value: &Value,
    path: &str,
    op: JsonPathOp,
    expected: Option<&str>,
) -> Result<()> {
    let segments = parse_path(path)?;
    let matches = eval_json_path(value, &segments);
    match op {
        JsonPathOp::Exists => {
            if matches.is_empty() {
                anyhow::bail!("jsonpath '{path}' missing");
            }
            Ok(())
        }
        JsonPathOp::NotExists => {
            if !matches.is_empty() {
                anyhow::bail!("jsonpath '{path}' should be missing");
            }
            Ok(())
        }
        JsonPathOp::Equals => {
            let expected = expected.context("missing expected value for equals")?;
            let expected_value = parse_expected(expected);
            for value in matches {
                if value == &expected_value {
                    return Ok(());
                }
            }
            anyhow::bail!("jsonpath '{path}' did not equal {expected}")
        }
        JsonPathOp::Contains => {
            let expected = expected.context("missing expected value for contains")?;
            for value in matches {
                if contains_value(value, expected) {
                    return Ok(());
                }
            }
            anyhow::bail!("jsonpath '{path}' did not contain {expected}")
        }
        JsonPathOp::Matches => {
            let expected = expected.context("missing expected value for matches")?;
            let regex =
                Regex::new(expected).with_context(|| format!("invalid regex '{expected}'"))?;
            for value in matches {
                if let Some(text) = value.as_str() {
                    if regex.is_match(text) {
                        return Ok(());
                    }
                } else if regex.is_match(&value.to_string()) {
                    return Ok(());
                }
            }
            anyhow::bail!("jsonpath '{path}' did not match {expected}")
        }
    }
}

fn contains_value(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(s) => s.contains(expected),
        Value::Array(items) => {
            let expected_value = parse_expected(expected);
            items.iter().any(|item| item == &expected_value)
        }
        _ => false,
    }
}

fn parse_expected(expected: &str) -> Value {
    serde_json::from_str(expected).unwrap_or_else(|_| Value::String(expected.to_string()))
}

fn parse_path(path: &str) -> Result<Vec<PathSegment>> {
    let mut segments = Vec::new();
    let mut chars = path.chars().peekable();
    let mut current = String::new();

    while let Some(ch) = chars.next() {
        match ch {
            '.' => {
                if !current.is_empty() {
                    segments.push(PathSegment::Key(current.clone()));
                    current.clear();
                }
            }
            '[' => {
                if !current.is_empty() {
                    segments.push(PathSegment::Key(current.clone()));
                    current.clear();
                }
                let mut inner = String::new();
                for next in chars.by_ref() {
                    if next == ']' {
                        break;
                    }
                    inner.push(next);
                }
                if inner == "*" {
                    segments.push(PathSegment::IndexWildcard);
                    continue;
                }
                if inner.starts_with('"') && inner.ends_with('"') && inner.len() >= 2 {
                    let key = &inner[1..inner.len() - 1];
                    segments.push(PathSegment::Key(key.to_string()));
                    continue;
                }
                let idx: usize = inner
                    .parse()
                    .with_context(|| format!("invalid index segment '[{inner}]' in '{path}'"))?;
                segments.push(PathSegment::Index(idx));
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        segments.push(PathSegment::Key(current));
    }
    if segments.is_empty() {
        anyhow::bail!("empty jsonpath");
    }
    Ok(segments)
}

fn eval_json_path<'a>(value: &'a Value, segments: &[PathSegment]) -> Vec<&'a Value> {
    let mut current = vec![value];
    for seg in segments {
        let mut next = Vec::new();
        for item in current {
            match seg {
                PathSegment::Key(key) => {
                    if let Some(obj) = item.as_object()
                        && let Some(val) = obj.get(key)
                    {
                        next.push(val);
                    }
                }
                PathSegment::Index(idx) => {
                    if let Some(array) = item.as_array()
                        && let Some(val) = array.get(*idx)
                    {
                        next.push(val);
                    }
                }
                PathSegment::IndexWildcard => {
                    if let Some(array) = item.as_array() {
                        for val in array {
                            next.push(val);
                        }
                    }
                }
            }
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonpath_equals() {
        let value = serde_json::json!({"a": {"b": [1, 2, 3]}});
        evaluate_jsonpath(&value, "a.b[1]", JsonPathOp::Equals, Some("2")).unwrap();
    }
}
