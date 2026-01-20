use std::path::{Path, PathBuf};

use greentic_integration_core::errors::CoreError;

#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    pub path: PathBuf,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Step {
    pub line_no: usize,
    pub raw: String,
    pub kind: StepKind,
}

#[derive(Debug, Clone)]
pub enum StepKind {
    Directive(Directive),
    Command(CommandLine),
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CommandLine {
    pub argv: Vec<String>,
    pub display: String,
}

#[derive(Debug, Clone)]
pub enum Directive {
    Set {
        key: String,
        value: String,
    },
    Env {
        key: String,
        value: String,
    },
    CaptureStdout {
        path: String,
    },
    CaptureJson {
        path: String,
    },
    ExpectExit {
        code: String,
    },
    ExpectStdoutContains {
        value: String,
    },
    ExpectStderrContains {
        value: String,
    },
    ExpectJsonPath {
        file: String,
        path: String,
        op: String,
        value: Option<String>,
    },
    Workdir {
        path: String,
    },
    Mkdir {
        path: String,
    },
    Write {
        path: String,
        content: String,
    },
    NormalizeJson {
        input: String,
        output: String,
    },
    DiffJson {
        left: String,
        right: String,
    },
    SaveArtifact {
        path: String,
    },
    TrySaveTrace {
        path: String,
    },
    FailDropStateWrite,
    FailDelayStateRead {
        ms: String,
    },
    FailAssetTransient {
        ratio: String,
    },
    FailDuplicateInteraction,
}

pub fn parse_gtest_file(path: &Path) -> Result<Scenario, CoreError> {
    let contents = std::fs::read_to_string(path).map_err(|err| CoreError::ParseError {
        line_no: 0,
        message: format!("failed to read {}: {err}", path.display()),
    })?;
    parse_gtest_contents(path, &contents)
}

fn parse_gtest_contents(path: &Path, contents: &str) -> Result<Scenario, CoreError> {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("scenario")
        .to_string();
    let lines: Vec<&str> = contents.lines().collect();
    let mut steps = Vec::new();
    let mut idx = 0;
    while idx < lines.len() {
        let raw_line = lines[idx];
        let line_no = idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            idx += 1;
            continue;
        }
        if trimmed == "#" || trimmed.starts_with("##") || trimmed.starts_with("# ") {
            idx += 1;
            continue;
        }
        let kind = if let Some(rest) = trimmed.strip_prefix('#') {
            parse_hash_directive(line_no, rest.trim(), raw_line, &lines, &mut idx)?
        } else {
            StepKind::Command(CommandLine {
                argv: tokenize_command(line_no, trimmed)?,
                display: trimmed.to_string(),
            })
        };
        steps.push(Step {
            line_no,
            raw: raw_line.to_string(),
            kind,
        });
        idx += 1;
    }
    Ok(Scenario {
        name,
        path: path.to_path_buf(),
        steps,
    })
}

fn parse_hash_directive(
    line_no: usize,
    input: &str,
    raw: &str,
    lines: &[&str],
    idx: &mut usize,
) -> Result<StepKind, CoreError> {
    if input.len() >= 5 && input[..5].eq_ignore_ascii_case("fail:") {
        let after = input[5..].trim();
        return parse_fail_directive(line_no, after);
    }
    let mut parts = input.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim();
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "SET" => {
            let (key, value) = parse_key_value(line_no, rest, raw)?;
            Ok(StepKind::Directive(Directive::Set { key, value }))
        }
        "ENV" => {
            let (key, value) = parse_key_value(line_no, rest, raw)?;
            Ok(StepKind::Directive(Directive::Env { key, value }))
        }
        "CAPTURE_STDOUT" => Ok(StepKind::Directive(Directive::CaptureStdout {
            path: parse_redirect(line_no, rest, "#CAPTURE_STDOUT")?,
        })),
        "CAPTURE_JSON" => Ok(StepKind::Directive(Directive::CaptureJson {
            path: parse_redirect(line_no, rest, "#CAPTURE_JSON")?,
        })),
        "RUN" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected command after #RUN".to_string(),
                });
            }
            Ok(StepKind::Command(CommandLine {
                argv: tokenize_command(line_no, rest)?,
                display: rest.to_string(),
            }))
        }
        "EXPECT_EXIT" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected exit code after #EXPECT_EXIT".to_string(),
                });
            }
            Ok(StepKind::Directive(Directive::ExpectExit {
                code: rest.to_string(),
            }))
        }
        "EXPECT_STDOUT_CONTAINS" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected value after #EXPECT_STDOUT_CONTAINS".to_string(),
                });
            }
            Ok(StepKind::Directive(Directive::ExpectStdoutContains {
                value: rest.to_string(),
            }))
        }
        "EXPECT_STDERR_CONTAINS" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected value after #EXPECT_STDERR_CONTAINS".to_string(),
                });
            }
            Ok(StepKind::Directive(Directive::ExpectStderrContains {
                value: rest.to_string(),
            }))
        }
        "EXPECT_JSONPATH" => Ok(StepKind::Directive(parse_expect_jsonpath(line_no, rest)?)),
        "WORKDIR" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected path after #WORKDIR".to_string(),
                });
            }
            Ok(StepKind::Directive(Directive::Workdir {
                path: rest.to_string(),
            }))
        }
        "MKDIR" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected path after #MKDIR".to_string(),
                });
            }
            Ok(StepKind::Directive(Directive::Mkdir {
                path: rest.to_string(),
            }))
        }
        "WRITE" => parse_write(line_no, rest, raw, lines, idx),
        "NORMALIZE_JSON" => Ok(StepKind::Directive(parse_normalize_json(line_no, rest)?)),
        "DIFF_JSON" => Ok(StepKind::Directive(parse_diff_json(line_no, rest)?)),
        "SAVE_ARTIFACT" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected path after #SAVE_ARTIFACT".to_string(),
                });
            }
            Ok(StepKind::Directive(Directive::SaveArtifact {
                path: rest.to_string(),
            }))
        }
        "TRY_SAVE_TRACE" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected path after #TRY_SAVE_TRACE".to_string(),
                });
            }
            Ok(StepKind::Directive(Directive::TrySaveTrace {
                path: rest.to_string(),
            }))
        }
        _ => Err(CoreError::ParseError {
            line_no,
            message: format!("unknown directive: {raw}"),
        }),
    }
}

fn parse_redirect(line_no: usize, rest: &str, label: &str) -> Result<String, CoreError> {
    let mut parts = rest.splitn(2, '>');
    let left = parts.next().unwrap_or("").trim();
    let right = parts.next().unwrap_or("").trim();
    if !left.is_empty() && !left.starts_with('>') {
        return Err(CoreError::ParseError {
            line_no,
            message: format!("expected '>' in {label}"),
        });
    }
    if right.is_empty() {
        return Err(CoreError::ParseError {
            line_no,
            message: format!("expected path after {label}"),
        });
    }
    Ok(right.to_string())
}

fn parse_expect_jsonpath(line_no: usize, rest: &str) -> Result<Directive, CoreError> {
    let mut parts = rest.splitn(4, char::is_whitespace);
    let file = parts.next().unwrap_or("").trim();
    let path = parts.next().unwrap_or("").trim();
    let op = parts.next().unwrap_or("").trim();
    let value = parts.next().map(str::trim).filter(|v| !v.is_empty());
    if file.is_empty() || path.is_empty() || op.is_empty() {
        return Err(CoreError::ParseError {
            line_no,
            message: "expected #EXPECT_JSONPATH <file> <jsonpath> <op> <value?>".to_string(),
        });
    }
    let value = match op {
        "exists" | "not_exists" => None,
        _ => value.map(|v| v.to_string()),
    };
    Ok(Directive::ExpectJsonPath {
        file: file.to_string(),
        path: path.to_string(),
        op: op.to_string(),
        value,
    })
}

fn parse_normalize_json(line_no: usize, rest: &str) -> Result<Directive, CoreError> {
    let mut parts = rest.splitn(2, '>');
    let input = parts.next().unwrap_or("").trim();
    let output = parts.next().unwrap_or("").trim();
    if input.is_empty() || output.is_empty() {
        return Err(CoreError::ParseError {
            line_no,
            message: "expected #NORMALIZE_JSON <in> > <out>".to_string(),
        });
    }
    Ok(Directive::NormalizeJson {
        input: input.to_string(),
        output: output.to_string(),
    })
}

fn parse_diff_json(line_no: usize, rest: &str) -> Result<Directive, CoreError> {
    let mut parts = rest.splitn(3, char::is_whitespace);
    let left = parts.next().unwrap_or("").trim();
    let right = parts.next().unwrap_or("").trim();
    if left.is_empty() || right.is_empty() {
        return Err(CoreError::ParseError {
            line_no,
            message: "expected #DIFF_JSON <a> <b>".to_string(),
        });
    }
    Ok(Directive::DiffJson {
        left: left.to_string(),
        right: right.to_string(),
    })
}

fn parse_fail_directive(line_no: usize, input: &str) -> Result<StepKind, CoreError> {
    let mut parts = input.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let rest = parts.next().unwrap_or("").trim();
    match name.as_str() {
        "drop_state_write" => Ok(StepKind::Directive(Directive::FailDropStateWrite)),
        "delay_state_read" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected delay in ms after #FAIL: delay_state_read".to_string(),
                });
            }
            Ok(StepKind::Directive(Directive::FailDelayStateRead {
                ms: rest.to_string(),
            }))
        }
        "asset_transient_failure" => {
            if rest.is_empty() {
                return Err(CoreError::ParseError {
                    line_no,
                    message: "expected ratio after #FAIL: asset_transient_failure".to_string(),
                });
            }
            Ok(StepKind::Directive(Directive::FailAssetTransient {
                ratio: rest.to_string(),
            }))
        }
        "duplicate_interaction" => Ok(StepKind::Directive(Directive::FailDuplicateInteraction)),
        _ => Err(CoreError::ParseError {
            line_no,
            message: format!("unknown failure directive '{input}'"),
        }),
    }
}

fn parse_write(
    line_no: usize,
    rest: &str,
    raw: &str,
    lines: &[&str],
    idx: &mut usize,
) -> Result<StepKind, CoreError> {
    let marker = "<<<EOF";
    let Some(pos) = rest.find(marker) else {
        return Err(CoreError::ParseError {
            line_no,
            message: format!("expected '{marker}' in '{raw}'"),
        });
    };
    let path = rest[..pos].trim();
    if path.is_empty() {
        return Err(CoreError::ParseError {
            line_no,
            message: format!("expected path in '{raw}'"),
        });
    }
    let mut content_lines = Vec::new();
    let mut cursor = *idx + 1;
    while cursor < lines.len() {
        let line = lines[cursor];
        if line == "EOF" {
            *idx = cursor;
            let mut content = content_lines.join("\n");
            if !content_lines.is_empty() {
                content.push('\n');
            }
            return Ok(StepKind::Directive(Directive::Write {
                path: path.to_string(),
                content,
            }));
        }
        content_lines.push(line.to_string());
        cursor += 1;
    }
    Err(CoreError::ParseError {
        line_no,
        message: "missing EOF terminator for #WRITE".to_string(),
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
                        Some(other) => current.push(other),
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
    fn parse_basic_directives() {
        let input = "#SET FOO=bar\n#RUN echo hi\n#EXPECT_EXIT 0\n#SAVE_ARTIFACT out.txt\n#FAIL: drop_state_write\n#EXPECT_JSONPATH data.json a.b[0] equals 3\n";
        let scenario = parse_gtest_contents(Path::new("test.gtest"), input).unwrap();
        assert_eq!(scenario.steps.len(), 6);
        match &scenario.steps[0].kind {
            StepKind::Directive(Directive::Set { key, value }) => {
                assert_eq!(key, "FOO");
                assert_eq!(value, "bar");
            }
            _ => panic!("expected set directive"),
        }
        match &scenario.steps[1].kind {
            StepKind::Command(cmd) => assert_eq!(cmd.argv, vec!["echo", "hi"]),
            _ => panic!("expected command"),
        }
    }

    #[test]
    fn parse_write_block() {
        let input = "#WRITE file.txt <<<EOF\nline1\nline2\nEOF\n";
        let scenario = parse_gtest_contents(Path::new("test.gtest"), input).unwrap();
        assert_eq!(scenario.steps.len(), 1);
        match &scenario.steps[0].kind {
            StepKind::Directive(Directive::Write { path, content }) => {
                assert_eq!(path, "file.txt");
                assert_eq!(content, "line1\nline2\n");
            }
            _ => panic!("expected write directive"),
        }
    }
}
