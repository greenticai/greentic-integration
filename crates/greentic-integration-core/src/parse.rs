//! Parser for .gtest scripts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::errors::CoreError;
use crate::model::{
    Assertion, AssertionKind, CommandLine, Directive, JsonAssertOp, JsonSource, Step, StepKind,
    TestPlan,
};

/// Parse a .gtest script file into a test plan.
pub fn parse_gtest_file(path: &Path) -> Result<TestPlan, CoreError> {
    let contents = std::fs::read_to_string(path).map_err(|err| CoreError::ParseError {
        line_no: 0,
        message: format!("failed to read {}: {err}", path.display()),
    })?;
    let mut stack = vec![canonicalize_for_stack(path)];
    parse_gtest_contents(path.to_path_buf(), &contents, &mut stack)
}

fn parse_gtest_contents(
    path: PathBuf,
    contents: &str,
    stack: &mut Vec<PathBuf>,
) -> Result<TestPlan, CoreError> {
    let mut steps = Vec::new();
    let lines: Vec<&str> = contents.lines().collect();
    let mut idx = 0;
    while idx < lines.len() {
        let raw_line = lines[idx];
        let line_no = idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            idx += 1;
            continue;
        }
        let kind = if let Some(directive) = trimmed.strip_prefix('@') {
            let mut parts = directive.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").trim();
            let rest = parts.next().unwrap_or("").trim();
            if name == "include" {
                if rest.is_empty() {
                    return Err(CoreError::ParseError {
                        line_no,
                        message: "expected path after @include".to_string(),
                    });
                }
                let include_path = resolve_include_path(&path, rest);
                let include_key = canonicalize_for_stack(&include_path);
                if stack.contains(&include_key) {
                    return Err(CoreError::ParseError {
                        line_no,
                        message: format!(
                            "include recursion detected at {}",
                            include_path.display()
                        ),
                    });
                }
                let contents = std::fs::read_to_string(&include_path).map_err(|err| {
                    CoreError::ParseError {
                        line_no,
                        message: format!("failed to read {}: {err}", include_path.display()),
                    }
                })?;
                stack.push(include_key);
                let mut nested = parse_gtest_contents(include_path, &contents, stack)?.steps;
                steps.append(&mut nested);
                stack.pop();
                idx += 1;
                continue;
            }
            StepKind::Directive(parse_directive(line_no, directive, trimmed)?)
        } else {
            parse_command_line(line_no, trimmed, &lines, &mut idx)?
        };
        steps.push(Step {
            path: path.clone(),
            line_no,
            raw: raw_line.to_string(),
            kind,
        });
        idx += 1;
    }
    Ok(TestPlan { path, steps })
}

fn parse_command_line(
    line_no: usize,
    trimmed: &str,
    lines: &[&str],
    idx: &mut usize,
) -> Result<StepKind, CoreError> {
    if let Some(marker_idx) = trimmed.find("<<") {
        let remainder = trimmed[marker_idx + 2..].trim();
        let token = remainder
            .trim_start_matches('\'')
            .trim_end_matches('\'')
            .trim();
        if token.is_empty() {
            return Err(CoreError::ParseError {
                line_no,
                message: "missing heredoc terminator".to_string(),
            });
        }
        let mut content_lines = Vec::new();
        let mut cursor = *idx + 1;
        while cursor < lines.len() {
            let line = lines[cursor];
            if line == token {
                *idx = cursor;
                let mut command = String::new();
                command.push_str(trimmed);
                command.push('\n');
                command.push_str(&content_lines.join("\n"));
                command.push('\n');
                command.push_str(token);
                return Ok(StepKind::Command(CommandLine {
                    argv: wrap_shell_command(command),
                }));
            }
            content_lines.push(line.to_string());
            cursor += 1;
        }
        return Err(CoreError::ParseError {
            line_no,
            message: "missing heredoc terminator".to_string(),
        });
    }
    Ok(StepKind::Command(CommandLine {
        argv: tokenize_command(line_no, trimmed)?,
    }))
}

fn wrap_shell_command(command: String) -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd".to_string(), "/C".to_string(), command]
    } else {
        vec!["sh".to_string(), "-c".to_string(), command]
    }
}

fn parse_directive(line_no: usize, input: &str, raw: &str) -> Result<Directive, CoreError> {
    let mut parts = input.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim();
    match name {
        "set" => parse_kv_directive(line_no, rest, raw, DirectiveKind::Set),
        "unset" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected name after @unset".to_string(),
                });
            }
            if !is_valid_ident(rest) {
                return Err(CoreError::ParseError {
                    line_no,
                    message: format!("invalid name '{rest}'"),
                });
            }
            Ok(Directive::Unset {
                key: rest.to_string(),
            })
        }
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
        "assert" => parse_assert(line_no, rest),
        "capture" => parse_ident_directive(line_no, rest, "@capture", DirectiveKind::Capture),
        "print" => parse_ident_directive(line_no, rest, "@print", DirectiveKind::Print),
        "debug" => parse_debug(line_no, rest),
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
        DirectiveKind::Set => {
            if let Some(command) = parse_set_command(&value) {
                Ok(Directive::SetCommand { key, command })
            } else {
                Ok(Directive::Set { key, value })
            }
        }
        DirectiveKind::Env => Ok(Directive::Env { key, value }),
        _ => Err(CoreError::ParseError {
            line_no,
            message: "invalid directive kind".to_string(),
        }),
    }
}

fn parse_set_command(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if let Some(command) = trimmed.strip_prefix("$(").and_then(|v| v.strip_suffix(')')) {
        return Some(command.trim().to_string());
    }
    None
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

fn parse_debug(line_no: usize, rest: &str) -> Result<Directive, CoreError> {
    if rest.trim() == "vars" {
        return Ok(Directive::DebugVars);
    }
    Err(CoreError::ParseError {
        line_no,
        message: "expected '@debug vars'".to_string(),
    })
}

fn parse_assert(line_no: usize, rest: &str) -> Result<Directive, CoreError> {
    if rest.trim().is_empty() {
        return Err(CoreError::ParseError {
            line_no,
            message: "expected assertion after @assert".to_string(),
        });
    }
    let tokens = tokenize_command(line_no, rest)?;
    let assertion = parse_assert_tokens(line_no, &tokens)?;
    Ok(Directive::Assert { assertion })
}

fn parse_assert_tokens(line_no: usize, tokens: &[String]) -> Result<Assertion, CoreError> {
    let mut iter = tokens.iter();
    let first = iter.next().ok_or_else(|| CoreError::ParseError {
        line_no,
        message: "missing assertion".to_string(),
    })?;
    if let Some(value) = first.strip_prefix("exit=") {
        let code: i32 = value.parse().map_err(|_| CoreError::ParseError {
            line_no,
            message: format!("invalid exit code '{value}'"),
        })?;
        return Ok(Assertion {
            kind: AssertionKind::Exit {
                equals: Some(code),
                not_equals: None,
            },
        });
    }
    if let Some(value) = first.strip_prefix("exit!=") {
        let code: i32 = value.parse().map_err(|_| CoreError::ParseError {
            line_no,
            message: format!("invalid exit code '{value}'"),
        })?;
        return Ok(Assertion {
            kind: AssertionKind::Exit {
                equals: None,
                not_equals: Some(code),
            },
        });
    }
    match first.as_str() {
        "stdout" => parse_assert_contains(line_no, iter, true),
        "stderr" => parse_assert_contains(line_no, iter, false),
        "file_exists" => {
            let path = iter.next().ok_or_else(|| CoreError::ParseError {
                line_no,
                message: "expected path after file_exists".to_string(),
            })?;
            Ok(Assertion {
                kind: AssertionKind::FileExists {
                    path: path.to_string(),
                },
            })
        }
        "file_not_exists" => {
            let path = iter.next().ok_or_else(|| CoreError::ParseError {
                line_no,
                message: "expected path after file_not_exists".to_string(),
            })?;
            Ok(Assertion {
                kind: AssertionKind::FileNotExists {
                    path: path.to_string(),
                },
            })
        }
        "jsonpath" => parse_assert_jsonpath(line_no, JsonSource::LastStdout, iter),
        "jsonfile" => {
            let file = iter.next().ok_or_else(|| CoreError::ParseError {
                line_no,
                message: "expected path after jsonfile".to_string(),
            })?;
            let next = iter.next().ok_or_else(|| CoreError::ParseError {
                line_no,
                message: "expected 'jsonpath' after jsonfile path".to_string(),
            })?;
            if next != "jsonpath" {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected 'jsonpath' after jsonfile path".to_string(),
                });
            }
            parse_assert_jsonpath(
                line_no,
                JsonSource::File {
                    path: file.to_string(),
                },
                iter,
            )
        }
        _ => Err(CoreError::ParseError {
            line_no,
            message: format!("invalid assertion '{first}'"),
        }),
    }
}

fn parse_assert_contains(
    line_no: usize,
    mut iter: std::slice::Iter<'_, String>,
    stdout: bool,
) -> Result<Assertion, CoreError> {
    let op = iter.next().ok_or_else(|| CoreError::ParseError {
        line_no,
        message: "expected 'contains' after stdout/stderr".to_string(),
    })?;
    if op != "contains" {
        return Err(CoreError::ParseError {
            line_no,
            message: "expected 'contains' after stdout/stderr".to_string(),
        });
    }
    let value = iter.next().ok_or_else(|| CoreError::ParseError {
        line_no,
        message: "expected value after contains".to_string(),
    })?;
    let value = std::iter::once(value.as_str())
        .chain(iter.map(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Assertion {
        kind: if stdout {
            AssertionKind::StdoutContains { value }
        } else {
            AssertionKind::StderrContains { value }
        },
    })
}

fn parse_assert_jsonpath(
    line_no: usize,
    source: JsonSource,
    mut iter: std::slice::Iter<'_, String>,
) -> Result<Assertion, CoreError> {
    let path = iter.next().ok_or_else(|| CoreError::ParseError {
        line_no,
        message: "expected jsonpath expression".to_string(),
    })?;
    let op = iter.next().ok_or_else(|| CoreError::ParseError {
        line_no,
        message: "expected jsonpath operator".to_string(),
    })?;
    let (op, value) = match op.as_str() {
        "==" => {
            let value = iter.next().ok_or_else(|| CoreError::ParseError {
                line_no,
                message: "expected value after '=='".to_string(),
            })?;
            let value = std::iter::once(value.as_str())
                .chain(iter.map(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join(" ");
            (JsonAssertOp::Equals, Some(value))
        }
        "exists" => (JsonAssertOp::Exists, None),
        "not_exists" => (JsonAssertOp::NotExists, None),
        _ => {
            return Err(CoreError::ParseError {
                line_no,
                message: format!("invalid jsonpath operator '{op}'"),
            });
        }
    };
    Ok(Assertion {
        kind: AssertionKind::JsonPath {
            source,
            path: path.to_string(),
            op,
            value,
        },
    })
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

fn resolve_include_path(base: &Path, include: &str) -> PathBuf {
    let path = PathBuf::from(include);
    if path.is_absolute() {
        return path;
    }
    let base_dir = base.parent().unwrap_or(base);
    base_dir.join(path)
}

fn canonicalize_for_stack(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
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
        let mut stack = Vec::new();
        let plan = parse_gtest_contents(PathBuf::from("test.gtest"), input, &mut stack).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].line_no, 1);
        assert_eq!(plan.steps[1].line_no, 4);
    }

    #[test]
    fn parse_include_recursion() {
        let base = PathBuf::from("root.gtest");
        let include = resolve_include_path(&base, "root.gtest");
        let key = canonicalize_for_stack(&include);
        let mut stack = vec![key];
        let err = parse_gtest_contents(base, "@include root.gtest\n", &mut stack).unwrap_err();
        match err {
            CoreError::ParseError { message, .. } => {
                assert!(message.contains("include recursion"));
            }
            _ => panic!("expected recursion parse error"),
        }
    }
}
