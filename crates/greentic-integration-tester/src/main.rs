use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use greentic_integration_core::errors::CoreError;
use greentic_integration_core::model::{
    CommandLine, Directive, Step, StepKind, SubstitutionContext,
};
use greentic_integration_core::parse::parse_gtest_file;
use greentic_integration_core::substitute::substitute;
use rayon::prelude::*;
use serde::Serialize;
use wait_timeout::ChildExt;
use walkdir::WalkDir;

const STDOUT_LIMIT: usize = 1024 * 1024;
const STDERR_LIMIT: usize = 1024 * 1024;
const TRANSCRIPT_LIMIT: usize = 8 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "greentic-integration-tester",
    about = "Run greentic integration .gtest scripts"
)]
struct Cli {
    /// Path to the .gtest script or directory
    #[arg(long, value_name = "PATH")]
    test: PathBuf,

    /// Working directory for command execution
    #[arg(long, value_name = "PATH")]
    workdir: Option<PathBuf>,

    /// Keep the working directory on success
    #[arg(long)]
    keep_workdir: bool,

    /// Repository root for built-in substitutions
    #[arg(long, value_name = "PATH")]
    repo_root: Option<PathBuf>,

    /// Prepend PATH entries for spawned commands
    #[arg(long, value_name = "PATHS")]
    prepend_path: Option<String>,

    /// Stop scheduling new tests after the first failure
    #[arg(long)]
    fail_fast: bool,

    /// Number of tests to run in parallel
    #[arg(long, value_name = "N", default_value_t = 1)]
    concurrency: usize,

    /// Report format
    #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
    report: ReportFormat,

    /// Write report output to a file
    #[arg(long, value_name = "PATH")]
    report_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ReportFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Serialize)]
struct ReportBundle {
    tests: Vec<TestReport>,
}

#[derive(Debug, Clone, Serialize)]
struct TestReport {
    test_path: String,
    workdir: String,
    start_ms: u64,
    end_ms: u64,
    status: String,
    steps: Vec<StepReport>,
    captures: BTreeMap<String, Capture>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
enum StepReport {
    Directive {
        line_no: usize,
        raw: String,
        directive: DirectiveReport,
    },
    Command {
        line_no: usize,
        raw: String,
        argv: Vec<String>,
        cwd: String,
        env_overrides: Vec<String>,
        exit: Option<i32>,
        stdout: String,
        stderr: String,
        duration_ms: u128,
        timed_out: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum DirectiveReport {
    Set {
        key: String,
        value: String,
    },
    Env {
        key: String,
        value: String,
    },
    Cd {
        path: String,
    },
    Timeout {
        duration_ms: u128,
    },
    ExpectExit {
        equals: Option<i32>,
        not_equals: Option<i32>,
    },
    Capture {
        name: String,
    },
    Print {
        name: String,
    },
    Skip {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize)]
struct Capture {
    name: String,
    argv: Vec<String>,
    cwd: String,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    duration_ms: u128,
    timed_out: bool,
}

#[derive(Debug)]
struct ExecutionContext {
    cwd: PathBuf,
    default_timeout: Option<Duration>,
    expect_next: Expectation,
    pending_capture: Option<String>,
    env_overrides: HashMap<String, String>,
    substitution: SubstitutionContext,
    skip_reason: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct Expectation {
    equals: Option<i32>,
    not_equals: Option<i32>,
}

impl Expectation {
    fn default_exit_zero() -> Self {
        Self {
            equals: Some(0),
            not_equals: None,
        }
    }

    fn matches(self, code: i32) -> bool {
        if let Some(eq) = self.equals {
            return code == eq;
        }
        if let Some(ne) = self.not_equals {
            return code != ne;
        }
        true
    }
}

#[derive(Debug)]
struct RunOutcome {
    report: TestReport,
    transcript: String,
    success: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = cli
        .repo_root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let tests = discover_tests(&cli.test)?;
    if tests.is_empty() {
        bail!("no .gtest files found under {}", cli.test.display());
    }

    let fail_fast = Arc::new(AtomicBool::new(false));
    let run_all = |tests: Vec<PathBuf>| -> Result<Vec<RunOutcome>> {
        if cli.concurrency <= 1 {
            let mut outcomes = Vec::with_capacity(tests.len());
            for test in tests {
                if cli.fail_fast && fail_fast.load(Ordering::SeqCst) {
                    break;
                }
                let outcome = run_single_test(&cli, &repo_root, &test)?;
                if !outcome.success && cli.fail_fast {
                    fail_fast.store(true, Ordering::SeqCst);
                }
                outcomes.push(outcome);
            }
            return Ok(outcomes);
        }

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(cli.concurrency)
            .build()
            .context("failed to build thread pool")?;
        let fail_fast = fail_fast.clone();
        let outcomes = pool.install(|| {
            tests
                .into_par_iter()
                .map(|test| {
                    if cli.fail_fast && fail_fast.load(Ordering::SeqCst) {
                        return Ok(None);
                    }
                    let outcome = run_single_test(&cli, &repo_root, &test)?;
                    if !outcome.success && cli.fail_fast {
                        fail_fast.store(true, Ordering::SeqCst);
                    }
                    Ok(Some(outcome))
                })
                .collect::<Result<Vec<Option<RunOutcome>>>>()
        })?;
        Ok(outcomes.into_iter().flatten().collect())
    };

    let mut outcomes = run_all(tests)?;
    outcomes.sort_by(|a, b| a.report.test_path.cmp(&b.report.test_path));

    let bundle = ReportBundle {
        tests: outcomes.iter().map(|o| o.report.clone()).collect(),
    };

    match cli.report {
        ReportFormat::Text => {
            let mut combined = String::new();
            for outcome in &outcomes {
                combined.push_str(&outcome.transcript);
                if !combined.ends_with('\n') {
                    combined.push('\n');
                }
            }
            write_report(&cli, &combined)?;
        }
        ReportFormat::Json => {
            let payload = serde_json::to_string_pretty(&bundle)?;
            write_report(&cli, &payload)?;
        }
    }

    let any_failed = outcomes.iter().any(|o| !o.success);
    if any_failed {
        bail!("one or more tests failed");
    }
    Ok(())
}

fn write_report(cli: &Cli, output: &str) -> Result<()> {
    if let Some(path) = &cli.report_file {
        std::fs::write(path, output)
            .with_context(|| format!("failed to write report {}", path.display()))?;
    } else {
        println!("{output}");
    }
    Ok(())
}

fn discover_tests(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut tests = Vec::new();
    for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
        let entry_path = entry.path();
        if entry_path.is_file()
            && entry_path
                .extension()
                .map(|ext| ext == "gtest")
                .unwrap_or(false)
        {
            tests.push(entry_path.to_path_buf());
        }
    }
    Ok(tests)
}

fn run_single_test(cli: &Cli, repo_root: &Path, test_path: &Path) -> Result<RunOutcome> {
    let plan = parse_gtest_file(test_path).map_err(|err| anyhow::anyhow!("{err}"))?;
    let test_dir = test_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root.to_path_buf());
    let (workdir, keep_workdir) = prepare_workdir(cli, test_path)?;
    let tmp_dir = workdir.join("tmp");
    std::fs::create_dir_all(&tmp_dir)?;

    let mut ctx = ExecutionContext {
        cwd: workdir.clone(),
        default_timeout: None,
        expect_next: Expectation::default_exit_zero(),
        pending_capture: None,
        env_overrides: HashMap::new(),
        substitution: SubstitutionContext::default(),
        skip_reason: None,
    };

    ctx.substitution.builtin.insert(
        "TEST_DIR".to_string(),
        test_dir.to_string_lossy().into_owned(),
    );
    ctx.substitution.builtin.insert(
        "WORK_DIR".to_string(),
        workdir.to_string_lossy().into_owned(),
    );
    ctx.substitution.builtin.insert(
        "REPO_ROOT".to_string(),
        repo_root.to_string_lossy().into_owned(),
    );
    ctx.substitution.builtin.insert(
        "TMP_DIR".to_string(),
        tmp_dir.to_string_lossy().into_owned(),
    );

    let start_ms = now_ms();
    let mut transcript = String::new();
    let mut captures = BTreeMap::new();
    let mut step_reports = Vec::new();
    let mut success = true;
    let mut last_cwd = ctx.cwd.clone();

    for step in plan.steps.iter() {
        match &step.kind {
            StepKind::Directive(directive) => {
                let report = handle_directive(step, directive, &mut ctx, &mut captures)?;
                step_reports.push(StepReport::Directive {
                    line_no: step.line_no,
                    raw: step.raw.clone(),
                    directive: report,
                });
                if ctx.skip_reason.is_some() {
                    success = true;
                    break;
                }
            }
            StepKind::Command(command) => {
                let run = execute_command(step, command, cli, &mut ctx)?;
                if last_cwd != ctx.cwd {
                    transcript.push_str(&format!("cwd: {}\n", ctx.cwd.to_string_lossy()));
                    last_cwd = ctx.cwd.clone();
                }
                transcript.push_str(&format!("[{}] $ {}\n", step.line_no, run.argv.join(" ")));
                transcript.push_str(&format!(
                    "exit: {} ({} ms)\n",
                    run.exit.unwrap_or(-1),
                    run.duration_ms
                ));
                transcript.push_str(&format!(
                    "stdout:\n{}\n",
                    truncate_for_transcript(&run.stdout)
                ));
                transcript.push_str(&format!(
                    "stderr:\n{}\n",
                    truncate_for_transcript(&run.stderr)
                ));
                if run.timed_out {
                    transcript.push_str("timed_out: true\n");
                }
                step_reports.push(StepReport::Command {
                    line_no: step.line_no,
                    raw: step.raw.clone(),
                    argv: run.argv.clone(),
                    cwd: run.cwd.clone(),
                    env_overrides: run.env_override_keys.clone(),
                    exit: run.exit,
                    stdout: run.stdout.clone(),
                    stderr: run.stderr.clone(),
                    duration_ms: run.duration_ms,
                    timed_out: run.timed_out,
                });
                if let Some(name) = run.capture_name.clone() {
                    captures.insert(
                        name.clone(),
                        Capture {
                            name,
                            argv: run.argv.clone(),
                            cwd: run.cwd.clone(),
                            exit_code: run.exit,
                            stdout: run.stdout.clone(),
                            stderr: run.stderr.clone(),
                            duration_ms: run.duration_ms,
                            timed_out: run.timed_out,
                        },
                    );
                }
                if !run.passed_expectation {
                    success = false;
                    transcript.push_str(&format!(
                        "failure: expectation not met at line {}\n",
                        step.line_no
                    ));
                    break;
                }
            }
        }
    }

    let end_ms = now_ms();
    if let Some(reason) = ctx.skip_reason.as_ref() {
        transcript.push_str(&format!("status: skipped ({reason})\n"));
        success = true;
    } else if success {
        transcript.push_str("status: ok\n");
    } else {
        transcript.push_str(&format!(
            "status: failed (workdir: {})\n",
            workdir.to_string_lossy()
        ));
    }

    if success && ctx.skip_reason.is_none() && !keep_workdir {
        let _ = std::fs::remove_dir_all(&workdir);
    }

    let report = TestReport {
        test_path: test_path.to_string_lossy().into_owned(),
        workdir: workdir.to_string_lossy().into_owned(),
        start_ms,
        end_ms,
        status: if ctx.skip_reason.is_some() {
            "skipped".into()
        } else if success {
            "ok".into()
        } else {
            "failed".into()
        },
        steps: step_reports,
        captures,
    };

    Ok(RunOutcome {
        report,
        transcript,
        success,
    })
}

fn prepare_workdir(cli: &Cli, test_path: &Path) -> Result<(PathBuf, bool)> {
    if let Some(base) = &cli.workdir {
        let name = sanitize_name(
            test_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test"),
        );
        let dir = base.join(name);
        std::fs::create_dir_all(&dir)?;
        return Ok((dir, cli.keep_workdir));
    }
    let dir = tempfile::Builder::new()
        .prefix("gtest-")
        .tempdir()
        .context("failed to create temp workdir")?;
    let path = dir.keep();
    Ok((path, cli.keep_workdir))
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn handle_directive(
    step: &Step,
    directive: &Directive,
    ctx: &mut ExecutionContext,
    captures: &mut BTreeMap<String, Capture>,
) -> Result<DirectiveReport> {
    match directive {
        Directive::Set { key, value } => {
            let value = substitute(value, &ctx.substitution, step.line_no)?;
            ctx.substitution
                .test_vars
                .insert(key.clone(), value.clone());
            Ok(DirectiveReport::Set {
                key: key.clone(),
                value,
            })
        }
        Directive::Env { key, value } => {
            let value = substitute(value, &ctx.substitution, step.line_no)?;
            ctx.substitution.env_vars.insert(key.clone(), value.clone());
            ctx.env_overrides.insert(key.clone(), value.clone());
            Ok(DirectiveReport::Env {
                key: key.clone(),
                value,
            })
        }
        Directive::Cd { path } => {
            let path = substitute(path, &ctx.substitution, step.line_no)?;
            let next = if Path::new(&path).is_absolute() {
                PathBuf::from(&path)
            } else {
                ctx.cwd.join(&path)
            };
            ctx.cwd = next;
            Ok(DirectiveReport::Cd { path })
        }
        Directive::Timeout { duration } => {
            ctx.default_timeout = Some(*duration);
            Ok(DirectiveReport::Timeout {
                duration_ms: duration.as_millis(),
            })
        }
        Directive::ExpectExit { equals, not_equals } => {
            ctx.expect_next = Expectation {
                equals: *equals,
                not_equals: *not_equals,
            };
            Ok(DirectiveReport::ExpectExit {
                equals: *equals,
                not_equals: *not_equals,
            })
        }
        Directive::Capture { name } => {
            let name = substitute(name, &ctx.substitution, step.line_no)?;
            ctx.pending_capture = Some(name.clone());
            Ok(DirectiveReport::Capture { name })
        }
        Directive::Print { name } => {
            let name = substitute(name, &ctx.substitution, step.line_no)?;
            if let Some(capture) = captures.get(&name) {
                println!(
                    "capture {name}:\nstdout:\n{}\nstderr:\n{}",
                    capture.stdout, capture.stderr
                );
                Ok(DirectiveReport::Print { name })
            } else {
                Err(CoreError::ParseError {
                    line_no: step.line_no,
                    message: format!("missing capture '{name}'"),
                }
                .into())
            }
        }
        Directive::Skip { reason } => {
            let reason = substitute(reason, &ctx.substitution, step.line_no)?;
            ctx.skip_reason = Some(reason.clone());
            Ok(DirectiveReport::Skip { reason })
        }
    }
}

struct CommandRun {
    argv: Vec<String>,
    cwd: String,
    env_override_keys: Vec<String>,
    exit: Option<i32>,
    stdout: String,
    stderr: String,
    duration_ms: u128,
    timed_out: bool,
    capture_name: Option<String>,
    passed_expectation: bool,
}

fn execute_command(
    step: &Step,
    command: &CommandLine,
    cli: &Cli,
    ctx: &mut ExecutionContext,
) -> Result<CommandRun> {
    let mut argv = Vec::with_capacity(command.argv.len());
    for token in &command.argv {
        argv.push(substitute(token, &ctx.substitution, step.line_no)?);
    }
    let resolved = resolve_command_path(&argv[0])?;
    let mut cmd = Command::new(&resolved);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    cmd.current_dir(&ctx.cwd);
    let mut envs: HashMap<OsString, OsString> = std::env::vars_os().collect();
    for (key, value) in &ctx.env_overrides {
        envs.insert(OsString::from(key), OsString::from(value));
    }
    if let Some(prepend) = &cli.prepend_path {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let mut path_value = prepend.clone();
        if let Some(existing) = envs.get(&OsString::from("PATH")) {
            if !path_value.is_empty() {
                path_value.push_str(sep);
            }
            path_value.push_str(&existing.to_string_lossy());
        }
        envs.insert(OsString::from("PATH"), OsString::from(path_value));
    }
    cmd.env_clear();
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let start = Instant::now();
    let mut child = cmd.spawn().with_context(|| {
        format!(
            "failed to spawn command '{}' at line {}",
            argv[0], step.line_no
        )
    })?;

    let stdout_handle = child
        .stdout
        .take()
        .map(|stdout| std::thread::spawn(move || read_limited(stdout, STDOUT_LIMIT)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stderr| std::thread::spawn(move || read_limited(stderr, STDERR_LIMIT)));

    let timeout = ctx.default_timeout;
    let (timed_out, exit_status) = if let Some(timeout) = timeout {
        match child.wait_timeout(timeout)? {
            Some(status) => (false, Some(status)),
            None => {
                let _ = child.kill();
                let status = child.wait().ok();
                (true, status)
            }
        }
    } else {
        let status = child.wait().ok();
        (false, status)
    };
    let exit = exit_status.and_then(|s| s.code());

    let stdout = stdout_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_else(Vec::new);
    let stderr = stderr_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_else(Vec::new);

    let stdout = String::from_utf8_lossy(&stdout).to_string();
    let stderr = String::from_utf8_lossy(&stderr).to_string();
    let duration_ms = start.elapsed().as_millis();

    let expectation = ctx.expect_next;
    ctx.expect_next = Expectation::default_exit_zero();
    let passed = if let Some(code) = exit {
        expectation.matches(code)
    } else {
        false
    };

    let capture_name = ctx.pending_capture.take();

    Ok(CommandRun {
        argv,
        cwd: ctx.cwd.to_string_lossy().into_owned(),
        env_override_keys: redact_env_keys(&ctx.env_overrides),
        exit,
        stdout,
        stderr,
        duration_ms,
        timed_out,
        capture_name,
        passed_expectation: passed,
    })
}

fn read_limited<R: Read>(mut reader: R, limit: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut remaining = limit;
    loop {
        let read_len = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if remaining > 0 {
            let take = remaining.min(read_len);
            buf.extend_from_slice(&chunk[..take]);
            remaining -= take;
        }
    }
    buf
}

fn resolve_command_path(command: &str) -> Result<PathBuf> {
    if command.contains(std::path::MAIN_SEPARATOR) || command.contains('/') || command.contains('\\') {
        return Ok(PathBuf::from(command));
    }
    if command == "greentic-integration-validator" {
        let exe = std::env::current_exe().context("failed to resolve tester executable path")?;
        let exe_dir = exe
            .parent()
            .context("tester executable has no parent directory")?;
        let mut candidate = exe_dir.join(command);
        if cfg!(windows) {
            candidate.set_extension("exe");
        }
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Ok(PathBuf::from(command))
}

fn truncate_for_transcript(value: &str) -> String {
    if value.len() <= TRANSCRIPT_LIMIT {
        return value.to_string();
    }
    let mut truncated = value[..TRANSCRIPT_LIMIT].to_string();
    truncated.push_str("\n...[truncated]...");
    truncated
}

fn redact_env_keys(env: &HashMap<String, String>) -> Vec<String> {
    let mut keys: Vec<String> = env.keys().cloned().collect();
    keys.sort();
    keys
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis() as u64
}
