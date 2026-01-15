use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use regex::Regex;
use serde_cbor::Value as CborValue;
use serde_json::Value as JsonValue;

#[derive(Debug, Parser)]
#[command(
    name = "greentic-integration-validator",
    about = "Validation helpers for .gtest scripts"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// File-related assertions
    File {
        #[command(subcommand)]
        cmd: FileCommand,
    },
    /// JSON-related assertions
    Json {
        #[command(subcommand)]
        cmd: JsonCommand,
    },
    /// CBOR-related assertions
    Cbor {
        #[command(subcommand)]
        cmd: CborCommand,
    },
}

#[derive(Debug, Subcommand)]
enum FileCommand {
    /// Assert a file exists
    Exists { path: PathBuf },
    /// Assert a file does not exist
    NotExists { path: PathBuf },
    /// Assert a file contains a substring
    Contains { path: PathBuf, substring: String },
    /// Assert a file matches a regex
    Regex { path: PathBuf, pattern: String },
}

#[derive(Debug, Subcommand)]
enum JsonCommand {
    /// Query a JSON path
    Path(JsonPathArgs),
}

#[derive(Debug, Args)]
struct JsonPathArgs {
    file: PathBuf,
    path: String,
    #[arg(long)]
    exists: bool,
    #[arg(long)]
    eq: Option<String>,
}

#[derive(Debug, Subcommand)]
enum CborCommand {
    /// Query a CBOR path
    Path(CborPathArgs),
    /// Recursive search within CBOR
    Find(CborFindArgs),
}

#[derive(Debug, Args)]
struct CborPathArgs {
    file: PathBuf,
    path: String,
    #[arg(long)]
    exists: bool,
    #[arg(long)]
    eq: Option<String>,
    #[arg(long)]
    all: bool,
    #[arg(long, value_name = "N")]
    count: Option<usize>,
}

#[derive(Debug, Args)]
struct CborFindArgs {
    file: PathBuf,
    #[arg(long)]
    key: Option<String>,
    #[arg(long)]
    string: Option<String>,
    #[arg(long)]
    regex: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSegment {
    Key(String),
    Index(usize),
    IndexWildcard,
}

fn main() {
    let cli = Cli::parse();
    let exit_code = match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("internal error: {err:#}");
            1
        }
    };
    std::process::exit(exit_code);
}

fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::File { cmd } => handle_file(cmd),
        Command::Json { cmd } => handle_json(cmd),
        Command::Cbor { cmd } => handle_cbor(cmd),
    }
}

fn handle_file(cmd: FileCommand) -> Result<i32> {
    match cmd {
        FileCommand::Exists { path } => {
            report_bool(exists(&path)?, format!("{} does not exist", path.display()))
        }
        FileCommand::NotExists { path } => {
            report_bool(!exists(&path)?, format!("{} exists", path.display()))
        }
        FileCommand::Contains { path, substring } => {
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            report_bool(
                contents.contains(&substring),
                format!("{} missing substring", path.display()),
            )
        }
        FileCommand::Regex { path, pattern } => {
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let regex = Regex::new(&pattern)?;
            report_bool(
                regex.is_match(&contents),
                format!("{} regex no match", path.display()),
            )
        }
    }
}

fn handle_json(cmd: JsonCommand) -> Result<i32> {
    match cmd {
        JsonCommand::Path(args) => {
            let data = fs::read_to_string(&args.file)
                .with_context(|| format!("failed to read {}", args.file.display()))?;
            let value: JsonValue = serde_json::from_str(&data).context("failed to parse JSON")?;
            let segments = parse_path(&args.path)?;
            let matches = eval_json_path(&value, &segments);
            evaluate_path_matches(matches, args.exists, args.eq.as_deref())
        }
    }
}

fn handle_cbor(cmd: CborCommand) -> Result<i32> {
    match cmd {
        CborCommand::Path(args) => {
            let bytes = fs::read(&args.file)
                .with_context(|| format!("failed to read {}", args.file.display()))?;
            let value: CborValue =
                serde_cbor::from_slice(&bytes).context("failed to parse CBOR")?;
            let segments = parse_path(&args.path)?;
            let matches = eval_cbor_path(&value, &segments);
            evaluate_cbor_matches(
                matches,
                args.exists,
                args.eq.as_deref(),
                args.all,
                args.count,
            )
        }
        CborCommand::Find(args) => {
            let bytes = fs::read(&args.file)
                .with_context(|| format!("failed to read {}", args.file.display()))?;
            let value: CborValue =
                serde_cbor::from_slice(&bytes).context("failed to parse CBOR")?;
            let found = cbor_find(&value, &args)?;
            report_bool(found, "CBOR find assertion failed".to_string())
        }
    }
}

fn report_bool(pass: bool, message: String) -> Result<i32> {
    if pass {
        Ok(0)
    } else {
        eprintln!("validation failed: {message}");
        Ok(2)
    }
}

fn exists(path: &PathBuf) -> Result<bool> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("failed to access {}", path.display())),
    }
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
                let idx: usize = inner.parse().with_context(|| {
                    format!("invalid index segment '[{inner}]' in path '{path}'")
                })?;
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
        bail!("empty path");
    }
    Ok(segments)
}

fn eval_json_path<'a>(value: &'a JsonValue, segments: &[PathSegment]) -> Vec<&'a JsonValue> {
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

fn eval_cbor_path(value: &CborValue, segments: &[PathSegment]) -> Vec<CborValue> {
    let mut current = vec![value.clone()];
    for seg in segments {
        let mut next = Vec::new();
        for item in current {
            match seg {
                PathSegment::Key(key) => {
                    if let CborValue::Map(map) = item {
                        for (k, v) in map {
                            if let CborValue::Text(kstr) = k
                                && kstr == *key
                            {
                                next.push(v);
                            }
                        }
                    }
                }
                PathSegment::Index(idx) => {
                    if let CborValue::Array(array) = item
                        && let Some(val) = array.get(*idx)
                    {
                        next.push(val.clone());
                    }
                }
                PathSegment::IndexWildcard => {
                    if let CborValue::Array(array) = item {
                        next.extend(array.into_iter());
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

fn evaluate_path_matches(matches: Vec<&JsonValue>, exists: bool, eq: Option<&str>) -> Result<i32> {
    if !exists && eq.is_none() {
        bail!("must provide --exists or --eq");
    }
    if exists && !matches.is_empty() {
        return Ok(0);
    }
    if let Some(eq_str) = eq {
        let expected: JsonValue =
            serde_json::from_str(eq_str).context("failed to parse JSON literal for --eq")?;
        let mut matched = false;
        for value in matches {
            if value == &expected {
                matched = true;
                break;
            }
        }
        return report_bool(matched, "JSON value mismatch".to_string());
    }
    report_bool(false, "JSON path missing".to_string())
}

fn evaluate_cbor_matches(
    matches: Vec<CborValue>,
    exists: bool,
    eq: Option<&str>,
    require_all: bool,
    count: Option<usize>,
) -> Result<i32> {
    if !exists && eq.is_none() {
        bail!("must provide --exists or --eq");
    }
    if let Some(expected_count) = count
        && matches.len() != expected_count
    {
        return report_bool(
            false,
            format!("expected {expected_count} matches, got {}", matches.len()),
        );
    }
    if exists && !matches.is_empty() && eq.is_none() {
        return Ok(0);
    }
    if let Some(eq_str) = eq {
        let expected: JsonValue =
            serde_json::from_str(eq_str).context("failed to parse JSON literal for --eq")?;
        let mut comparisons = Vec::new();
        for value in matches {
            let json = cbor_to_json(&value)?;
            comparisons.push(json == expected);
        }
        let matched = if require_all {
            comparisons.iter().all(|v| *v)
        } else {
            comparisons.iter().any(|v| *v)
        };
        return report_bool(matched, "CBOR value mismatch".to_string());
    }
    report_bool(!matches.is_empty(), "CBOR path missing".to_string())
}

fn cbor_to_json(value: &CborValue) -> Result<JsonValue> {
    Ok(match value {
        CborValue::Null => JsonValue::Null,
        CborValue::Bool(v) => JsonValue::Bool(*v),
        CborValue::Integer(v) => {
            let number = if *v >= 0 {
                let value: u64 = (*v).try_into().context("CBOR integer out of JSON range")?;
                serde_json::Number::from(value)
            } else {
                let value: i64 = (*v).try_into().context("CBOR integer out of JSON range")?;
                serde_json::Number::from(value)
            };
            JsonValue::Number(number)
        }
        CborValue::Float(v) => {
            let number = serde_json::Number::from_f64(*v).context("invalid float for JSON")?;
            JsonValue::Number(number)
        }
        CborValue::Bytes(_) => {
            bail!("bytes compare not supported; use --type bytes or cbor find --bytes-hex")
        }
        CborValue::Text(s) => JsonValue::String(s.clone()),
        CborValue::Array(arr) => {
            let mut items = Vec::with_capacity(arr.len());
            for item in arr {
                items.push(cbor_to_json(item)?);
            }
            JsonValue::Array(items)
        }
        CborValue::Map(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let key = match k {
                    CborValue::Text(s) => s.clone(),
                    _ => continue,
                };
                out.insert(key, cbor_to_json(v)?);
            }
            JsonValue::Object(out)
        }
        CborValue::Tag(_, inner) => cbor_to_json(inner)?,
        _ => bail!("unsupported CBOR value"),
    })
}

fn cbor_find(root: &CborValue, args: &CborFindArgs) -> Result<bool> {
    let mut wanted = 0;
    if args.key.is_some() {
        wanted += 1;
    }
    if args.string.is_some() {
        wanted += 1;
    }
    if args.regex.is_some() {
        wanted += 1;
    }
    if wanted != 1 {
        bail!("must provide exactly one of --key, --string, or --regex");
    }
    let regex = match &args.regex {
        Some(pattern) => Some(Regex::new(pattern)?),
        None => None,
    };
    Ok(find_in_cbor(root, &args.key, &args.string, &regex))
}

fn find_in_cbor(
    value: &CborValue,
    key: &Option<String>,
    string: &Option<String>,
    regex: &Option<Regex>,
) -> bool {
    match value {
        CborValue::Map(map) => {
            for (k, v) in map {
                if let CborValue::Text(kstr) = k
                    && let Some(target) = key
                    && kstr == target
                {
                    return true;
                }
                if find_in_cbor(v, key, string, regex) {
                    return true;
                }
            }
            false
        }
        CborValue::Array(arr) => arr.iter().any(|v| find_in_cbor(v, key, string, regex)),
        CborValue::Text(s) => {
            if let Some(target) = string
                && s == target
            {
                return true;
            }
            if let Some(re) = regex
                && re.is_match(s)
            {
                return true;
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_path_segments() {
        let segments = parse_path(r#"a.b[0].c["x-y"]"#).unwrap();
        assert_eq!(
            segments,
            vec![
                PathSegment::Key("a".into()),
                PathSegment::Key("b".into()),
                PathSegment::Index(0),
                PathSegment::Key("c".into()),
                PathSegment::Key("x-y".into())
            ]
        );
    }

    #[test]
    fn json_path_eval() {
        let value: JsonValue = serde_json::json!({
            "a": {"b": [{"c": 42}]}
        });
        let segments = parse_path("a.b[0].c").unwrap();
        let matches = eval_json_path(&value, &segments);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], &JsonValue::from(42));
    }

    #[test]
    fn cbor_path_eval() {
        let value: CborValue = serde_cbor::value::to_value(serde_json::json!({
            "items": [{"name": "Ada"}, {"name": "Bob"}]
        }))
        .unwrap();
        let segments = parse_path("items[1].name").unwrap();
        let matches = eval_cbor_path(&value, &segments);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], CborValue::Text("Bob".into()));
    }

    #[test]
    fn cbor_find_string() {
        let value: CborValue = serde_cbor::value::to_value(serde_json::json!({
            "items": [{"name": "Ada"}, {"name": "Bob"}]
        }))
        .unwrap();
        let args = CborFindArgs {
            file: PathBuf::from("dummy"),
            key: None,
            string: Some("Bob".into()),
            regex: None,
        };
        let found = cbor_find(&value, &args).unwrap();
        assert!(found);
    }
}
