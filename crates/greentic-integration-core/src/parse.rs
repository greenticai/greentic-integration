//! Parser for .gtest scripts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::errors::CoreError;
use crate::model::{CommandLine, Directive, Step, StepKind, TestPlan};

/// Parse a .gtest script file into a test plan.
pub fn parse_gtest_file(path: &Path) -> Result<TestPlan, CoreError> {
    let contents = std::fs::read_to_string(path).map_err(|err| CoreError::ParseError {
        line_no: 0,
        message: format!("failed to read {}: {err}", path.display()),
    })?;
    parse_gtest_contents(path.to_path_buf(), &contents)
}

fn parse_gtest_contents(path: PathBuf, contents: &str) -> Result<TestPlan, CoreError> {
    let mut steps = Vec::new();
    for (idx, raw_line) in contents.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let kind = if let Some(rest) = trimmed.strip_prefix('@') {
            StepKind::Directive(parse_directive(line_no, rest, trimmed)?)
        } else {
            StepKind::Command(CommandLine {
                argv: tokenize_command(line_no, trimmed)?,
            })
        };
        steps.push(Step {
            line_no,
            raw: raw_line.to_string(),
            kind,
        });
    }
    Ok(TestPlan { path, steps })
}

fn parse_directive(line_no: usize, input: &str, raw: &str) -> Result<Directive, CoreError> {
    let mut parts = input.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim();
    match name {
        "set" => parse_kv_directive(line_no, rest, raw, DirectiveKind::Set),
        "env" => parse_kv_directive(line_no, rest, raw, DirectiveKind::Env),
        "cd" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected path after @cd".to_string(),
                });
            }
            Ok(Directive::Cd {
                path: rest.to_string(),
            })
        }
        "timeout" => parse_timeout(line_no, rest),
        "expect" => parse_expect(line_no, rest),
        "capture" => parse_ident_directive(line_no, rest, "@capture", DirectiveKind::Capture),
        "print" => parse_ident_directive(line_no, rest, "@print", DirectiveKind::Print),
        "skip" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected reason after @skip".to_string(),
                });
            }
            Ok(Directive::Skip {
                reason: rest.to_string(),
            })
        }
        _ => Err(CoreError::ParseError {
            line_no,
            message: format!("unknown directive: {raw}"),
        }),
    }
}

enum DirectiveKind {
    Set,
    Env,
    Capture,
    Print,
}

fn parse_kv_directive(
    line_no: usize,
    rest: &str,
    raw: &str,
    kind: DirectiveKind,
) -> Result<Directive, CoreError> {
    let (key, value) = parse_key_value(line_no, rest, raw)?;
    match kind {
        DirectiveKind::Set => Ok(Directive::Set { key, value }),
        DirectiveKind::Env => Ok(Directive::Env { key, value }),
        _ => Err(CoreError::ParseError {
            line_no,
            message: "invalid directive kind".to_string(),
        }),
    }
}

fn parse_ident_directive(
    line_no: usize,
    rest: &str,
    label: &str,
    kind: DirectiveKind,
) -> Result<Directive, CoreError> {
    if rest.is_empty() {
        return Err(CoreError::ParseError {
            line_no,
            message: format!("expected name after {label}"),
        });
    }
    if !is_valid_ident(rest) {
        return Err(CoreError::ParseError {
            line_no,
            message: format!("invalid name '{rest}'"),
        });
    }
    match kind {
        DirectiveKind::Capture => Ok(Directive::Capture {
            name: rest.to_string(),
        }),
        DirectiveKind::Print => Ok(Directive::Print {
            name: rest.to_string(),
        }),
        _ => Err(CoreError::ParseError {
            line_no,
            message: "invalid directive kind".to_string(),
        }),
    }
}

fn parse_key_value(line_no: usize, rest: &str, raw: &str) -> Result<(String, String), CoreError> {
    let mut iter = rest.splitn(2, '=');
    let key = iter.next().unwrap_or("").trim();
    let value = match iter.next() {
        Some(value) => value,
        None => {
            return Err(CoreError::ParseError {
                line_no,
                message: format!("expected KEY=VALUE in '{raw}'"),
            });
        }
    };
    if key.is_empty() {
        return Err(CoreError::ParseError {
            line_no,
            message: format!("expected KEY=VALUE in '{raw}'"),
        });
    }
    if !is_valid_ident(key) {
        return Err(CoreError::ParseError {
            line_no,
            message: format!("invalid key '{key}'"),
        });
    }
    Ok((key.to_string(), value.to_string()))
}

fn parse_timeout(line_no: usize, rest: &str) -> Result<Directive, CoreError> {
    if rest.is_empty() {
        return Err(CoreError::ParseError {
            line_no,
            message: "expected duration after @timeout".to_string(),
        });
    }
    let (num_str, unit) = if rest.ends_with("ms") {
        rest.split_at(rest.len() - 2)
    } else {
        rest.split_at(rest.len().saturating_sub(1))
    };
    if unit != "ms" && unit != "s" && unit != "m" && unit != "h" {
        return Err(CoreError::ParseError {
            line_no,
            message: format!("invalid duration '{rest}'"),
        });
    }
    let value: u64 = num_str.parse().map_err(|_| CoreError::ParseError {
        line_no,
        message: format!("invalid duration '{rest}'"),
    })?;
    let duration = match unit {
        "ms" => Duration::from_millis(value),
        "s" => Duration::from_secs(value),
        "m" => Duration::from_secs(value * 60),
        "h" => Duration::from_secs(value * 60 * 60),
        _ => {
            return Err(CoreError::ParseError {
                line_no,
                message: format!("invalid duration '{rest}'"),
            });
        }
    };
    Ok(Directive::Timeout { duration })
}

fn parse_expect(line_no: usize, rest: &str) -> Result<Directive, CoreError> {
    let trimmed = rest.trim();
    if let Some(value) = trimmed.strip_prefix("exit=") {
        let code: i32 = value.parse().map_err(|_| CoreError::ParseError {
            line_no,
            message: format!("invalid exit code '{value}'"),
        })?;
        return Ok(Directive::ExpectExit {
            equals: Some(code),
            not_equals: None,
        });
    }
    if let Some(value) = trimmed.strip_prefix("exit!=") {
        let code: i32 = value.parse().map_err(|_| CoreError::ParseError {
            line_no,
            message: format!("invalid exit code '{value}'"),
        })?;
        return Ok(Directive::ExpectExit {
            equals: None,
            not_equals: Some(code),
        });
    }
    Err(CoreError::ParseError {
        line_no,
        message: format!("invalid expect directive '{trimmed}'"),
    })
}

fn tokenize_command(line_no: usize, input: &str) -> Result<Vec<String>, CoreError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    enum Mode {
        Normal,
        Single,
        Double,
    }
    let mut mode = Mode::Normal;
    while let Some(ch) = chars.next() {
        match mode {
            Mode::Normal => {
                if ch.is_whitespace() {
                    if !current.is_empty() {
                        tokens.push(current.clone());
                        current.clear();
                    }
                } else if ch == '\'' {
                    mode = Mode::Single;
                } else if ch == '"' {
                    mode = Mode::Double;
                } else {
                    current.push(ch);
                }
            }
            Mode::Single => {
                if ch == '\'' {
                    mode = Mode::Normal;
                } else {
                    current.push(ch);
                }
            }
            Mode::Double => {
                if ch == '"' {
                    mode = Mode::Normal;
                } else if ch == '\\' {
                    match chars.next() {
                        Some('"') => current.push('"'),
                        Some('n') => current.push('\n'),
                        Some(other) => {
                            current.push(other);
                        }
                        None => {
                            return Err(CoreError::TokenizeError {
                                line_no,
                                message: "unterminated escape".to_string(),
                            });
                        }
                    }
                } else {
                    current.push(ch);
                }
            }
        }
    }
    match mode {
        Mode::Normal => {}
        Mode::Single => {
            return Err(CoreError::TokenizeError {
                line_no,
                message: "unterminated single quote".to_string(),
            });
        }
        Mode::Double => {
            return Err(CoreError::TokenizeError {
                line_no,
                message: "unterminated double quote".to_string(),
            });
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    if tokens.is_empty() {
        return Err(CoreError::TokenizeError {
            line_no,
            message: "empty command line".to_string(),
        });
    }
    Ok(tokens)
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
    fn tokenize_basic() {
        let argv = tokenize_command(1, "echo hello world").unwrap();
        assert_eq!(argv, vec!["echo", "hello", "world"]);
    }

    #[test]
    fn tokenize_quotes() {
        let argv = tokenize_command(1, "echo \"hello world\" 'and more'").unwrap();
        assert_eq!(argv, vec!["echo", "hello world", "and more"]);
    }

    #[test]
    fn tokenize_double_quote_escapes() {
        let argv = tokenize_command(1, "echo \"line\\nnext\"").unwrap();
        assert_eq!(argv, vec!["echo", "line\nnext"]);
    }

    #[test]
    fn parse_directives() {
        let directive = parse_directive(3, "set FOO=bar", "@set FOO=bar").unwrap();
        match directive {
            Directive::Set { key, value } => {
                assert_eq!(key, "FOO");
                assert_eq!(value, "bar");
            }
            _ => panic!("expected set directive"),
        }
        let directive = parse_directive(4, "timeout 500ms", "@timeout 500ms").unwrap();
        match directive {
            Directive::Timeout { duration } => {
                assert_eq!(duration, Duration::from_millis(500));
            }
            _ => panic!("expected timeout directive"),
        }
    }

    #[test]
    fn parse_gtest_lines() {
        let input = "@set FOO=bar\n\n# comment\nls -la\n";
        let plan = parse_gtest_contents(PathBuf::from("test.gtest"), input).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].line_no, 1);
        assert_eq!(plan.steps[1].line_no, 4);
    }
}
