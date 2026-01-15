//! Substitution for templated values.

use crate::errors::CoreError;
use crate::model::SubstitutionContext;

/// Apply substitutions to an input string.
pub fn substitute(
    input: &str,
    ctx: &SubstitutionContext,
    line_no: usize,
) -> Result<String, CoreError> {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && matches!(chars.peek(), Some('{')) {
            chars.next();
            let mut var = String::new();
            let mut closed = false;
            while let Some(&next) = chars.peek() {
                if next == '}' {
                    chars.next();
                    closed = true;
                    break;
                }
                var.push(next);
                chars.next();
            }
            if !closed {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "unterminated ${...} expression".to_string(),
                });
            }
            if var.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "empty variable name".to_string(),
                });
            }
            if !is_valid_ident(&var) {
                return Err(CoreError::ParseError {
                    line_no,
                    message: format!("invalid variable name '{var}'"),
                });
            }
            let value = lookup_var(ctx, &var).ok_or(CoreError::MissingVar { line_no, var })?;
            out.push_str(&value);
            continue;
        }
        out.push(ch);
    }
    Ok(out)
}

fn lookup_var(ctx: &SubstitutionContext, name: &str) -> Option<String> {
    if let Some(value) = ctx.test_vars.get(name) {
        return Some(value.clone());
    }
    if let Some(value) = ctx.env_vars.get(name) {
        return Some(value.clone());
    }
    if let Ok(value) = std::env::var(name) {
        return Some(value);
    }
    ctx.builtin.get(name).cloned()
}

fn is_valid_ident(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_precedence() {
        let mut ctx = SubstitutionContext::default();
        ctx.test_vars.insert("FOO".to_string(), "test".to_string());
        ctx.env_vars.insert("FOO".to_string(), "env".to_string());
        ctx.builtin.insert("FOO".to_string(), "builtin".to_string());
        let out = substitute("value=${FOO}", &ctx, 1).unwrap();
        assert_eq!(out, "value=test");
    }

    #[test]
    fn substitute_missing_var() {
        let ctx = SubstitutionContext::default();
        let err = substitute("value=${MISSING}", &ctx, 3).unwrap_err();
        match err {
            CoreError::MissingVar { line_no, var } => {
                assert_eq!(line_no, 3);
                assert_eq!(var, "MISSING");
            }
            _ => panic!("expected missing var error"),
        }
    }

    #[test]
    fn substitute_multiple() {
        let mut ctx = SubstitutionContext::default();
        ctx.test_vars.insert("A".to_string(), "1".to_string());
        ctx.test_vars.insert("B".to_string(), "2".to_string());
        let out = substitute("${A}-${B}", &ctx, 1).unwrap();
        assert_eq!(out, "1-2");
    }

    #[test]
    fn substitute_no_changes() {
        let ctx = SubstitutionContext::default();
        let out = substitute("plain", &ctx, 1).unwrap();
        assert_eq!(out, "plain");
    }
}
