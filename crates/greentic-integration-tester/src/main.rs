use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use greentic_integration_core::errors::CoreError;
use greentic_integration_core::model::{
    Assertion, AssertionKind, CommandLine, Directive, JsonAssertOp, JsonSource, Step, StepKind,
    SubstitutionContext, TestPlan,
};
use greentic_integration_core::parse::parse_gtest_file as parse_legacy_gtest_file;
use greentic_integration_core::substitute::substitute;
use rayon::prelude::*;
use serde::Serialize;
use wait_timeout::ChildExt;
use walkdir::WalkDir;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
mod gtest;
mod json;
mod junit;

const STDOUT_LIMIT: usize = 1024 * 1024;
const STDERR_LIMIT: usize = 1024 * 1024;
const TRANSCRIPT_LIMIT: usize = 8 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "greentic-integration-tester",
    about = "Run greentic integration .gtest scripts"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
    #[command(flatten)]
    legacy: LegacyArgs,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Run .gtest scripts with the MVP runner
    Run(RunArgs),
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Path to the .gtest script or directory
    #[arg(long, value_name = "PATH")]
    gtest: PathBuf,

    /// Directory for per-step artifacts
    #[arg(long, value_name = "PATH")]
    artifacts_dir: Option<PathBuf>,

    /// Write JUnit XML report to a file
    #[arg(long, value_name = "PATH")]
    junit: Option<PathBuf>,

    /// Working directory for command execution
    #[arg(long, value_name = "PATH")]
    workdir: Option<PathBuf>,

    /// Keep the working directory on success
    #[arg(long)]
    keep_workdir: bool,

    /// Prepend PATH entries for spawned commands
    #[arg(long, value_name = "PATHS")]
    prepend_path: Option<String>,

    /// Seed for deterministic failure injection
    #[arg(long, value_name = "SEED")]
    seed: Option<u64>,

    /// Summarize errors for failing scenarios to stderr
    #[arg(long)]
    errors: bool,

    /// Normalization config for JSON directives
    #[arg(long, value_name = "PATH")]
    normalize_config: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct LegacyArgs {
    /// Path to the .gtest script or directory
    #[arg(long, value_name = "PATH")]
    test: Option<PathBuf>,

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

    /// Rerun failing tests to triage flakes
    #[arg(long)]
    triage_flakes: bool,

    /// Number of reruns for flake triage
    #[arg(long, value_name = "N", default_value_t = 3)]
    triage_runs: u32,

    /// Summarize errors for failing tests to stderr
    #[arg(long)]
    errors: bool,
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
        path: String,
        line_no: usize,
        raw: String,
        directive: DirectiveReport,
    },
    Command {
        path: String,
        line_no: usize,
        raw: String,
        argv: Vec<String>,
        cwd: String,
        env_overrides: Vec<String>,
        exit: Option<i32>,
        exit_description: String,
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
    SetCommand {
        key: String,
        command: String,
        value: String,
    },
    Unset {
        key: String,
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
    Assert {
        assertion: String,
    },
    Capture {
        name: String,
    },
    Print {
        name: String,
    },
    DebugVars {
        vars: Vec<String>,
    },
    Skip {
        reason: String,
    },
    Error {
        message: String,
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
    last_run: Option<CommandRun>,
    timeout_multiplier: f64,
    trace_dir: Option<PathBuf>,
}

#[derive(Debug)]
struct DirectiveOutcome {
    report: DirectiveReport,
    failure: Option<String>,
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

#[derive(Debug, Clone)]
struct RunConfig {
    triage: bool,
    timeout_multiplier: f64,
    trace_dir: Option<PathBuf>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            triage: false,
            timeout_multiplier: 1.0,
            trace_dir: None,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(CliCommand::Run(args)) = cli.command {
        return run_new(args);
    }
    let legacy = &cli.legacy;
    let test = legacy.test.as_ref().context("missing --test argument")?;
    let repo_root = legacy
        .repo_root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let tests = discover_tests(test)?;
    if tests.is_empty() {
        bail!("no .gtest files found under {}", test.display());
    }

    let fail_fast = Arc::new(AtomicBool::new(false));
    let run_all = |tests: Vec<PathBuf>| -> Result<Vec<RunOutcome>> {
        if legacy.concurrency <= 1 {
            let mut outcomes = Vec::with_capacity(tests.len());
            for test in tests {
                if legacy.fail_fast && fail_fast.load(Ordering::SeqCst) {
                    break;
                }
                let outcome = run_single_test(legacy, &repo_root, &test)?;
                if !outcome.success && legacy.fail_fast {
                    fail_fast.store(true, Ordering::SeqCst);
                }
                outcomes.push(outcome);
            }
            return Ok(outcomes);
        }

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(legacy.concurrency)
            .build()
            .context("failed to build thread pool")?;
        let fail_fast = fail_fast.clone();
        let outcomes = pool.install(|| {
            tests
                .into_par_iter()
                .map(|test| {
                    if legacy.fail_fast && fail_fast.load(Ordering::SeqCst) {
                        return Ok(None);
                    }
                    let outcome = run_single_test(legacy, &repo_root, &test)?;
                    if !outcome.success && legacy.fail_fast {
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

    match legacy.report {
        ReportFormat::Text => {
            let mut combined = String::new();
            for outcome in &outcomes {
                combined.push_str(&outcome.transcript);
                if !combined.ends_with('\n') {
                    combined.push('\n');
                }
            }
            write_report(legacy, &combined)?;
        }
        ReportFormat::Json => {
            let payload = serde_json::to_string_pretty(&bundle)?;
            write_report(legacy, &payload)?;
        }
    }

    let any_failed = outcomes.iter().any(|o| !o.success);
    if any_failed {
        if legacy.errors {
            emit_error_summary(&outcomes);
        }
        bail!("one or more tests failed");
    }
    Ok(())
}

fn run_new(args: RunArgs) -> Result<()> {
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let tests = discover_tests(&args.gtest)?;
    if tests.is_empty() {
        bail!("no .gtest files found under {}", args.gtest.display());
    }
    let normalize_config = json::normalize::load_config(args.normalize_config.as_deref())?;
    let mut scenarios = Vec::with_capacity(tests.len());
    for test_path in tests {
        let scenario =
            gtest::parse_gtest_file(&test_path).map_err(|err| anyhow::anyhow!("{err}"))?;
        scenarios.push(scenario);
    }
    let results = gtest::run_scenarios(
        scenarios,
        gtest::RunOptions {
            workdir: args.workdir,
            keep_workdir: args.keep_workdir,
            repo_root,
            prepend_path: args.prepend_path,
            artifacts_dir: args.artifacts_dir.clone(),
            seed: args.seed,
            normalize_config,
        },
    )?;
    if let Some(path) = args.junit.as_ref() {
        junit::write_junit(path, "gtest", &results)?;
    }
    for result in &results {
        if result.status == gtest::ScenarioStatus::Failed
            && let Some(hint) = result.replay_hint.as_ref()
        {
            println!("{hint}");
        }
    }
    let any_failed = results
        .iter()
        .any(|result| result.status == gtest::ScenarioStatus::Failed);
    if any_failed {
        if args.errors {
            emit_scenario_error_summary(&results);
        }
        bail!("one or more scenarios failed");
    }
    Ok(())
}

fn write_report(cli: &LegacyArgs, output: &str) -> Result<()> {
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

fn run_single_test(cli: &LegacyArgs, repo_root: &Path, test_path: &Path) -> Result<RunOutcome> {
    let plan = parse_legacy_gtest_file(test_path).map_err(|err| anyhow::anyhow!("{err}"))?;
    let mut outcome = run_plan(&plan, cli, repo_root, test_path, &RunConfig::default())?;
    if !outcome.success && cli.triage_flakes {
        triage_flake(cli, repo_root, test_path, &plan, &mut outcome)?;
    }
    Ok(outcome)
}

fn run_plan(
    plan: &TestPlan,
    cli: &LegacyArgs,
    repo_root: &Path,
    test_path: &Path,
    config: &RunConfig,
) -> Result<RunOutcome> {
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
        last_run: None,
        timeout_multiplier: config.timeout_multiplier,
        trace_dir: config.trace_dir.clone(),
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

    if config.triage {
        ctx.env_overrides
            .insert("RUST_LOG".to_string(), "debug".to_string());
        ctx.env_overrides
            .insert("GREENTIC_LOG".to_string(), "debug".to_string());
        if ctx.default_timeout.is_none() {
            ctx.default_timeout = Some(Duration::from_secs(120));
        }
        if let Some(dir) = ctx.trace_dir.as_ref() {
            std::fs::create_dir_all(dir)?;
        }
    }

    let start_ms = now_ms();
    let mut transcript = String::new();
    let mut captures = BTreeMap::new();
    let mut step_reports = Vec::new();
    let mut success = true;
    let mut last_cwd = ctx.cwd.clone();
    let mut step_index = 0usize;

    for step in plan.steps.iter() {
        match &step.kind {
            StepKind::Directive(directive) => {
                match handle_directive(step, directive, cli, &mut ctx, &mut captures) {
                    Ok(outcome) => {
                        step_reports.push(StepReport::Directive {
                            path: step.path.to_string_lossy().into_owned(),
                            line_no: step.line_no,
                            raw: step.raw.clone(),
                            directive: outcome.report,
                        });
                        if ctx.skip_reason.is_some() {
                            success = true;
                            break;
                        }
                        if let Some(failure) = outcome.failure {
                            success = false;
                            transcript.push_str(&format!(
                                "failure: assertion failed at {}:{}: {} ({})\n",
                                step.path.display(),
                                step.line_no,
                                step.raw.trim(),
                                failure
                            ));
                            break;
                        }
                    }
                    Err(err) => {
                        success = false;
                        transcript.push_str(&format!(
                            "failure: {}:{}: {}\n",
                            step.path.display(),
                            step.line_no,
                            step.raw
                        ));
                        transcript.push_str(&format!("error: {err}\n"));
                        step_reports.push(StepReport::Directive {
                            path: step.path.to_string_lossy().into_owned(),
                            line_no: step.line_no,
                            raw: step.raw.clone(),
                            directive: DirectiveReport::Error {
                                message: err.to_string(),
                            },
                        });
                        break;
                    }
                }
            }
            StepKind::Command(command) => {
                step_index += 1;
                let run = execute_command(step, command, cli, &mut ctx, step_index)?;
                if last_cwd != ctx.cwd {
                    transcript.push_str(&format!("cwd: {}\n", ctx.cwd.to_string_lossy()));
                    last_cwd = ctx.cwd.clone();
                }
                transcript.push_str(&format!("[{}] $ {}\n", step.line_no, run.argv.join(" ")));
                transcript.push_str(&format!(
                    "exit: {} ({} ms)\n",
                    run.exit_description, run.duration_ms
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
                    path: step.path.to_string_lossy().into_owned(),
                    line_no: step.line_no,
                    raw: step.raw.clone(),
                    argv: run.argv.clone(),
                    cwd: run.cwd.clone(),
                    env_overrides: run.env_override_keys.clone(),
                    exit: run.exit,
                    exit_description: run.exit_description.clone(),
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
                ctx.last_run = Some(run.clone());
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

#[derive(Debug, Serialize)]
struct TriageSummary {
    test_path: String,
    runs: u32,
    passes: u32,
    failures: u32,
    flaky: bool,
    minimized: Option<String>,
    artifacts_dir: String,
}

fn triage_flake(
    cli: &LegacyArgs,
    repo_root: &Path,
    test_path: &Path,
    plan: &TestPlan,
    outcome: &mut RunOutcome,
) -> Result<()> {
    let triage_dir = build_triage_dir(repo_root, test_path)?;
    std::fs::create_dir_all(&triage_dir)?;
    let original_path = triage_dir.join("original.gtest");
    let _ = std::fs::copy(test_path, &original_path);
    write_env_snapshot(&triage_dir)?;
    write_run_artifacts(&triage_dir, 0, outcome)?;

    let mut passes = 0u32;
    let mut failures = 0u32;

    for run_idx in 1..=cli.triage_runs {
        let trace_dir = triage_dir.join(format!("trace-run-{run_idx:02}"));
        let config = RunConfig {
            triage: true,
            timeout_multiplier: 2.0,
            trace_dir: Some(trace_dir),
        };
        let run = run_plan(plan, cli, repo_root, test_path, &config)?;
        write_run_artifacts(&triage_dir, run_idx, &run)?;
        if run.success {
            passes += 1;
        } else {
            failures += 1;
        }
    }

    let flaky = passes > 0;
    let minimized = minimize_failure(plan, cli, repo_root, test_path, &triage_dir)?;

    let summary = TriageSummary {
        test_path: test_path.to_string_lossy().into_owned(),
        runs: cli.triage_runs,
        passes,
        failures,
        flaky,
        minimized: minimized.as_ref().map(|p| p.to_string_lossy().into_owned()),
        artifacts_dir: triage_dir.to_string_lossy().into_owned(),
    };
    let summary_path = triage_dir.join("summary.json");
    std::fs::write(&summary_path, serde_json::to_string_pretty(&summary)?)?;

    outcome.transcript.push_str(&format!(
        "flake triage: passes={passes} failures={failures} flaky={flaky}\nartifacts: {}\n",
        triage_dir.to_string_lossy()
    ));
    if let Some(path) = minimized {
        outcome
            .transcript
            .push_str(&format!("minimized repro: {}\n", path.to_string_lossy()));
    }
    Ok(())
}

fn build_triage_dir(repo_root: &Path, test_path: &Path) -> Result<PathBuf> {
    let test_name = sanitize_name(
        test_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("test"),
    );
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs();
    Ok(repo_root
        .join("target")
        .join("flake-artifacts")
        .join(test_name)
        .join(timestamp.to_string()))
}

fn write_env_snapshot(dir: &Path) -> Result<()> {
    let mut lines: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    lines.sort();
    std::fs::write(dir.join("env.txt"), lines.join("\n"))?;
    Ok(())
}

fn write_run_artifacts(dir: &Path, run_idx: u32, outcome: &RunOutcome) -> Result<()> {
    let log_path = dir.join(format!("run-{run_idx:02}.log"));
    std::fs::write(&log_path, &outcome.transcript)?;
    let report_path = dir.join(format!("run-{run_idx:02}.json"));
    let payload = serde_json::to_string_pretty(&outcome.report)?;
    std::fs::write(report_path, payload)?;
    Ok(())
}

fn minimize_failure(
    plan: &TestPlan,
    cli: &LegacyArgs,
    repo_root: &Path,
    test_path: &Path,
    triage_dir: &Path,
) -> Result<Option<PathBuf>> {
    let command_count = plan
        .steps
        .iter()
        .filter(|step| matches!(step.kind, StepKind::Command(_)))
        .count();
    if command_count <= 1 || plan.steps.is_empty() {
        return Ok(None);
    }

    let mut low = 1usize;
    let mut high = plan.steps.len();
    let mut best: Option<usize> = None;
    while low <= high {
        let mid = (low + high) / 2;
        let candidate = TestPlan {
            path: plan.path.clone(),
            steps: plan.steps[..mid].to_vec(),
        };
        let trace_dir = triage_dir.join(format!("minimize-{mid:03}"));
        let config = RunConfig {
            triage: true,
            timeout_multiplier: 2.0,
            trace_dir: Some(trace_dir),
        };
        let run = run_plan(&candidate, cli, repo_root, test_path, &config)?;
        if run.success {
            low = mid + 1;
        } else {
            best = Some(mid);
            if mid == 1 {
                break;
            }
            high = mid - 1;
        }
    }

    let Some(best) = best else {
        return Ok(None);
    };
    let minimized_steps = plan.steps[..best].to_vec();
    let path = triage_dir.join("minimized.gtest");
    write_plan_file(&path, &minimized_steps)?;
    Ok(Some(path))
}

fn write_plan_file(path: &Path, steps: &[Step]) -> Result<()> {
    let mut contents = String::new();
    for step in steps {
        contents.push_str(&step.raw);
        contents.push('\n');
    }
    std::fs::write(path, contents)?;
    Ok(())
}

fn prepare_workdir(cli: &LegacyArgs, test_path: &Path) -> Result<(PathBuf, bool)> {
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
    cli: &LegacyArgs,
    ctx: &mut ExecutionContext,
    captures: &mut BTreeMap<String, Capture>,
) -> Result<DirectiveOutcome> {
    match directive {
        Directive::Set { key, value } => {
            let value = substitute(value, &ctx.substitution, step.line_no)?;
            ctx.substitution
                .test_vars
                .insert(key.clone(), value.clone());
            Ok(DirectiveOutcome {
                report: DirectiveReport::Set {
                    key: key.clone(),
                    value,
                },
                failure: None,
            })
        }
        Directive::SetCommand { key, command } => {
            let command = substitute(command, &ctx.substitution, step.line_no)?;
            let value = run_set_command(step.line_no, &command, cli, ctx)?;
            ctx.substitution
                .test_vars
                .insert(key.clone(), value.clone());
            Ok(DirectiveOutcome {
                report: DirectiveReport::SetCommand {
                    key: key.clone(),
                    command,
                    value,
                },
                failure: None,
            })
        }
        Directive::Unset { key } => {
            ctx.substitution.test_vars.remove(key);
            ctx.substitution.env_vars.remove(key);
            ctx.env_overrides.remove(key);
            Ok(DirectiveOutcome {
                report: DirectiveReport::Unset { key: key.clone() },
                failure: None,
            })
        }
        Directive::Env { key, value } => {
            let value = substitute(value, &ctx.substitution, step.line_no)?;
            ctx.substitution.env_vars.insert(key.clone(), value.clone());
            ctx.env_overrides.insert(key.clone(), value.clone());
            Ok(DirectiveOutcome {
                report: DirectiveReport::Env {
                    key: key.clone(),
                    value,
                },
                failure: None,
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
            Ok(DirectiveOutcome {
                report: DirectiveReport::Cd { path },
                failure: None,
            })
        }
        Directive::Timeout { duration } => {
            ctx.default_timeout = Some(*duration);
            Ok(DirectiveOutcome {
                report: DirectiveReport::Timeout {
                    duration_ms: duration.as_millis(),
                },
                failure: None,
            })
        }
        Directive::ExpectExit { equals, not_equals } => {
            ctx.expect_next = Expectation {
                equals: *equals,
                not_equals: *not_equals,
            };
            Ok(DirectiveOutcome {
                report: DirectiveReport::ExpectExit {
                    equals: *equals,
                    not_equals: *not_equals,
                },
                failure: None,
            })
        }
        Directive::Assert { assertion } => {
            let failure = apply_assertion(step, assertion, ctx)
                .err()
                .map(|err| err.to_string());
            Ok(DirectiveOutcome {
                report: DirectiveReport::Assert {
                    assertion: step.raw.clone(),
                },
                failure,
            })
        }
        Directive::Capture { name } => {
            let name = substitute(name, &ctx.substitution, step.line_no)?;
            ctx.pending_capture = Some(name.clone());
            Ok(DirectiveOutcome {
                report: DirectiveReport::Capture { name },
                failure: None,
            })
        }
        Directive::Print { name } => {
            let name = substitute(name, &ctx.substitution, step.line_no)?;
            if let Some(capture) = captures.get(&name) {
                println!(
                    "capture {name}:\nstdout:\n{}\nstderr:\n{}",
                    capture.stdout, capture.stderr
                );
                Ok(DirectiveOutcome {
                    report: DirectiveReport::Print { name },
                    failure: None,
                })
            } else {
                Err(CoreError::ParseError {
                    line_no: step.line_no,
                    message: format!("missing capture '{name}'"),
                }
                .into())
            }
        }
        Directive::DebugVars => {
            let mut pairs: Vec<String> = ctx
                .substitution
                .test_vars
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            pairs.sort();
            println!("vars:\n{}", pairs.join("\n"));
            Ok(DirectiveOutcome {
                report: DirectiveReport::DebugVars { vars: pairs },
                failure: None,
            })
        }
        Directive::Skip { reason } => {
            let reason = substitute(reason, &ctx.substitution, step.line_no)?;
            ctx.skip_reason = Some(reason.clone());
            Ok(DirectiveOutcome {
                report: DirectiveReport::Skip { reason },
                failure: None,
            })
        }
    }
}

fn run_set_command(
    line_no: usize,
    command: &str,
    cli: &LegacyArgs,
    ctx: &ExecutionContext,
) -> Result<String> {
    let argv = [
        if cfg!(windows) { "cmd" } else { "sh" }.to_string(),
        if cfg!(windows) { "/C" } else { "-c" }.to_string(),
        command.to_string(),
    ];
    let resolved = resolve_command_path(&argv[0])?;
    let mut cmd = Command::new(&resolved);
    cmd.args(&argv[1..]);
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

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "failed to spawn set command '{}' at line {}",
            argv.join(" "),
            line_no
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
    let status = child.wait().ok();
    let exit = status.and_then(|s| s.code());
    let stdout = stdout_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_else(Vec::new);
    let stderr = stderr_handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_else(Vec::new);
    let stdout = String::from_utf8_lossy(&stdout).to_string();
    let stderr = String::from_utf8_lossy(&stderr).to_string();
    if exit != Some(0) {
        return Err(anyhow::anyhow!(
            "set command failed (exit {:?}): {}",
            exit,
            stderr.trim()
        ));
    }
    Ok(stdout.trim_end_matches(['\n', '\r']).to_string())
}

fn apply_assertion(step: &Step, assertion: &Assertion, ctx: &ExecutionContext) -> Result<()> {
    match &assertion.kind {
        AssertionKind::Exit { equals, not_equals } => {
            let last_run = ctx
                .last_run
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no command output available for @assert exit"))?;
            let exit = last_run
                .exit
                .ok_or_else(|| anyhow::anyhow!("missing exit code"))?;
            if let Some(expected) = equals {
                if exit != *expected {
                    return Err(anyhow::anyhow!(
                        "assertion failed: exit expected {expected}, got {exit}"
                    ));
                }
                return Ok(());
            }
            if let Some(expected) = not_equals
                && exit == *expected
            {
                return Err(anyhow::anyhow!(
                    "assertion failed: exit not expected {expected}, got {exit}"
                ));
            }
            Ok(())
        }
        AssertionKind::StdoutContains { value } => {
            let value = substitute(value, &ctx.substitution, step.line_no)?;
            let last_run = ctx
                .last_run
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no command output available for @assert stdout"))?;
            if !last_run.stdout.contains(&value) {
                let actual = summarize_output(&last_run.stdout);
                return Err(anyhow::anyhow!(
                    "assertion failed: stdout missing '{value}' (actual: {actual})"
                ));
            }
            Ok(())
        }
        AssertionKind::StderrContains { value } => {
            let value = substitute(value, &ctx.substitution, step.line_no)?;
            let last_run = ctx
                .last_run
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no command output available for @assert stderr"))?;
            if !last_run.stderr.contains(&value) {
                let actual = summarize_output(&last_run.stderr);
                return Err(anyhow::anyhow!(
                    "assertion failed: stderr missing '{value}' (actual: {actual})"
                ));
            }
            Ok(())
        }
        AssertionKind::FileExists { path } => {
            let path = substitute(path, &ctx.substitution, step.line_no)?;
            let target = resolve_path(ctx, &path);
            if !target.exists() {
                return Err(anyhow::anyhow!(
                    "assertion failed: file does not exist ({})",
                    target.display()
                ));
            }
            Ok(())
        }
        AssertionKind::FileNotExists { path } => {
            let path = substitute(path, &ctx.substitution, step.line_no)?;
            let target = resolve_path(ctx, &path);
            if target.exists() {
                return Err(anyhow::anyhow!(
                    "assertion failed: file exists ({})",
                    target.display()
                ));
            }
            Ok(())
        }
        AssertionKind::JsonPath {
            source,
            path,
            op,
            value,
        } => {
            let path = substitute(path, &ctx.substitution, step.line_no)?;
            let value = value
                .as_ref()
                .map(|val| substitute(val, &ctx.substitution, step.line_no))
                .transpose()?;
            let json_value = match source {
                JsonSource::LastStdout => {
                    let last_run = ctx.last_run.as_ref().ok_or_else(|| {
                        anyhow::anyhow!("no command output available for @assert jsonpath")
                    })?;
                    serde_json::from_str(&last_run.stdout)
                        .map_err(|_| anyhow::anyhow!("stdout is not valid JSON"))?
                }
                JsonSource::File { path } => {
                    let file = substitute(path, &ctx.substitution, step.line_no)?;
                    let file_path = resolve_path(ctx, &file);
                    let raw = std::fs::read_to_string(&file_path)?;
                    serde_json::from_str(&raw)
                        .map_err(|_| anyhow::anyhow!("invalid JSON in {}", file_path.display()))?
                }
            };
            let matches = jsonpath_matches(&json_value, &path)?;
            match op {
                JsonAssertOp::Exists => {
                    if matches.is_empty() {
                        return Err(anyhow::anyhow!(
                            "assertion failed: jsonpath '{path}' missing (matches=0)"
                        ));
                    }
                }
                JsonAssertOp::NotExists => {
                    if !matches.is_empty() {
                        return Err(anyhow::anyhow!(
                            "assertion failed: jsonpath '{path}' should be missing (matches={})",
                            matches.len()
                        ));
                    }
                }
                JsonAssertOp::Equals => {
                    let expected = value
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("missing expected value"))?;
                    let expected_value = parse_expected_json(expected)?;
                    if !matches.iter().any(|item| item == &expected_value) {
                        let actual = summarize_json_matches(&matches);
                        return Err(anyhow::anyhow!(
                            "assertion failed: jsonpath '{path}' expected {expected}, actual {actual}"
                        ));
                    }
                }
            }
            Ok(())
        }
    }
}

fn summarize_output(value: &str) -> String {
    let trimmed = value.trim_end();
    if trimmed.len() <= 200 {
        return trimmed.to_string();
    }
    format!("{}...[truncated]", &trimmed[..200])
}

fn parse_expected_json(value: &str) -> Result<serde_json::Value> {
    Ok(serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.into())))
}

fn summarize_json_matches(matches: &[serde_json::Value]) -> String {
    let mut out = Vec::new();
    for item in matches.iter().take(3) {
        out.push(item.to_string());
    }
    if matches.len() > 3 {
        out.push("...".to_string());
    }
    format!("[{}]", out.join(", "))
}

fn resolve_path(ctx: &ExecutionContext, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.cwd.join(path)
    }
}

#[derive(Debug)]
enum JsonPathSegment {
    Key(String),
    Index(usize),
    IndexWildcard,
}

fn jsonpath_matches(value: &serde_json::Value, path: &str) -> Result<Vec<serde_json::Value>> {
    let segments = parse_jsonpath(path)?;
    let mut current = vec![value];
    for seg in segments {
        let mut next = Vec::new();
        for item in current {
            match seg {
                JsonPathSegment::Key(ref key) => {
                    if let Some(obj) = item.as_object()
                        && let Some(val) = obj.get(key.as_str())
                    {
                        next.push(val);
                    }
                }
                JsonPathSegment::Index(idx) => {
                    if let Some(array) = item.as_array()
                        && let Some(val) = array.get(idx)
                    {
                        next.push(val);
                    }
                }
                JsonPathSegment::IndexWildcard => {
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
    Ok(current.into_iter().cloned().collect())
}

fn parse_jsonpath(path: &str) -> Result<Vec<JsonPathSegment>> {
    let mut segments = Vec::new();
    let mut chars = path.chars().peekable();
    let mut current = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '.' => {
                if !current.is_empty() {
                    segments.push(JsonPathSegment::Key(current.clone()));
                    current.clear();
                }
            }
            '[' => {
                if !current.is_empty() {
                    segments.push(JsonPathSegment::Key(current.clone()));
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
                    segments.push(JsonPathSegment::IndexWildcard);
                    continue;
                }
                if inner.starts_with('"') && inner.ends_with('"') && inner.len() >= 2 {
                    let key = &inner[1..inner.len() - 1];
                    segments.push(JsonPathSegment::Key(key.to_string()));
                    continue;
                }
                let idx: usize = inner.parse().map_err(|_| {
                    anyhow::anyhow!("invalid index segment '[{inner}]' in path '{path}'")
                })?;
                segments.push(JsonPathSegment::Index(idx));
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        segments.push(JsonPathSegment::Key(current));
    }
    if segments.is_empty() {
        anyhow::bail!("empty jsonpath");
    }
    Ok(segments)
}

#[derive(Clone, Debug)]
struct CommandRun {
    argv: Vec<String>,
    cwd: String,
    env_override_keys: Vec<String>,
    exit: Option<i32>,
    exit_description: String,
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
    cli: &LegacyArgs,
    ctx: &mut ExecutionContext,
    step_index: usize,
) -> Result<CommandRun> {
    let mut argv = Vec::with_capacity(command.argv.len());
    for token in &command.argv {
        argv.push(substitute(token, &ctx.substitution, step.line_no)?);
    }
    let trace_path = ctx
        .trace_dir
        .as_ref()
        .and_then(|trace_dir| maybe_inject_trace(&mut argv, trace_dir, step_index));
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
    if let Some(path) = trace_path.as_ref() {
        envs.insert(
            OsString::from("GREENTIC_TRACE_OUT"),
            OsString::from(path.to_string_lossy().into_owned()),
        );
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

    let timeout = ctx.default_timeout.map(|duration| {
        if (ctx.timeout_multiplier - 1.0).abs() < f64::EPSILON {
            return duration;
        }
        let millis = duration.as_millis() as f64 * ctx.timeout_multiplier;
        Duration::from_millis(millis.max(1.0) as u64)
    });
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
    let exit = exit_status.as_ref().and_then(ExitStatus::code);
    let exit_description = format_exit_description(exit_status.as_ref(), timed_out);

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

    let run = CommandRun {
        argv,
        cwd: ctx.cwd.to_string_lossy().into_owned(),
        env_override_keys: redact_env_keys(&ctx.env_overrides),
        exit,
        exit_description,
        stdout,
        stderr,
        duration_ms,
        timed_out,
        capture_name,
        passed_expectation: passed,
    };
    ctx.last_run = Some(run.clone());
    Ok(run)
}

fn format_exit_description(status: Option<&ExitStatus>, timed_out: bool) -> String {
    if timed_out {
        return "timed out".to_string();
    }
    let Some(status) = status else {
        return "unknown".to_string();
    };
    if let Some(code) = status.code() {
        return code.to_string();
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return format!("signal {signal}");
    }
    "unknown".to_string()
}

fn emit_error_summary(outcomes: &[RunOutcome]) {
    let mut failures: Vec<&RunOutcome> = outcomes.iter().filter(|o| !o.success).collect();
    if failures.is_empty() {
        return;
    }
    failures.sort_by(|a, b| a.report.test_path.cmp(&b.report.test_path));
    eprintln!("gtest error summary:");
    for outcome in failures {
        let details = extract_failure_details(&outcome.transcript);
        if details.is_empty() {
            eprintln!("- {}: failed", outcome.report.test_path);
        } else {
            eprintln!("- {}: {}", outcome.report.test_path, details.join(" | "));
        }
    }
}

fn extract_failure_details(transcript: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in transcript.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("failure:")
            || trimmed.starts_with("error:")
            || trimmed.starts_with("status: failed")
        {
            lines.push(trimmed.to_string());
        }
    }
    if lines.is_empty()
        && let Some(last) = transcript
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
    {
        lines.push(last.trim().to_string());
    }
    if lines.len() > 3 {
        lines.split_off(lines.len() - 3)
    } else {
        lines
    }
}

fn emit_scenario_error_summary(results: &[gtest::ScenarioResult]) {
    let mut failures: Vec<&gtest::ScenarioResult> = results
        .iter()
        .filter(|result| result.status == gtest::ScenarioStatus::Failed)
        .collect();
    if failures.is_empty() {
        return;
    }
    failures.sort_by(|a, b| a.path.cmp(&b.path));
    eprintln!("gtest error summary:");
    for result in failures {
        let mut line = if let Some(failure) = &result.failure {
            format!(
                "- {}:{}: {}",
                result.path.display(),
                failure.line_no,
                collapse_ws(&failure.message)
            )
        } else {
            format!("- {}: failed", result.path.display())
        };
        if let Some(hint) = &result.replay_hint {
            line.push_str(" | replay: ");
            line.push_str(&collapse_ws(hint));
        }
        eprintln!("{line}");
    }
}

fn collapse_ws(value: &str) -> String {
    value
        .split_whitespace()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
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
    if command.contains(std::path::MAIN_SEPARATOR)
        || command.contains('/')
        || command.contains('\\')
    {
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

fn maybe_inject_trace(
    argv: &mut Vec<String>,
    trace_dir: &Path,
    step_index: usize,
) -> Option<PathBuf> {
    let command = argv.first()?;
    let file_name = Path::new(command)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(command);
    if file_name != "greentic-runner" && file_name != "greentic-runner-cli" {
        return None;
    }
    let has_trace_out = argv
        .iter()
        .any(|arg| arg == "--trace-out" || arg.starts_with("--trace-out=") || arg == "--trace");
    if has_trace_out {
        return None;
    }
    let trace_path = trace_dir.join(format!("trace-step-{step_index:03}.json"));
    argv.push("--trace-out".to_string());
    argv.push(trace_path.to_string_lossy().into_owned());
    Some(trace_path)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn build_legacy_args(test: &Path) -> LegacyArgs {
        LegacyArgs {
            test: Some(test.to_path_buf()),
            workdir: None,
            keep_workdir: true,
            repo_root: Some(
                test.parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(".")),
            ),
            prepend_path: None,
            fail_fast: false,
            concurrency: 1,
            report: ReportFormat::Text,
            report_file: None,
            triage_flakes: false,
            triage_runs: 3,
            errors: false,
        }
    }

    #[test]
    fn gtest_assertions_and_set_command() {
        if cfg!(windows) {
            return;
        }
        let dir = tempdir().unwrap();
        let gtest_path = dir.path().join("assertions.gtest");
        let contents = r#"@set NAME=world
@set FROM_CMD=$(printf "cmd")
sh -c "printf '{\"name\":\"${NAME}\",\"from\":\"${FROM_CMD}\"}'"
@assert stdout contains world
@assert jsonpath name == world
sh -c "printf 'data' > ${WORK_DIR}/out.txt"
@assert file_exists ${WORK_DIR}/out.txt
@assert file_not_exists ${WORK_DIR}/missing.txt
"#;
        std::fs::write(&gtest_path, contents).unwrap();

        let args = build_legacy_args(&gtest_path);
        let outcome = run_single_test(&args, dir.path(), &gtest_path).unwrap();
        assert!(outcome.success);
    }

    #[test]
    fn gtest_assertion_failure_message() {
        if cfg!(windows) {
            return;
        }
        let dir = tempdir().unwrap();
        let gtest_path = dir.path().join("fail.gtest");
        let contents = r#"@set NAME=world
printf "{\"name\":\"${NAME}\"}"
@assert jsonpath name == nope
"#;
        std::fs::write(&gtest_path, contents).unwrap();

        let args = build_legacy_args(&gtest_path);
        let outcome = run_single_test(&args, dir.path(), &gtest_path).unwrap();
        assert!(!outcome.success);
        assert!(outcome.transcript.contains("assertion failed"));
        assert!(outcome.transcript.contains("fail.gtest"));
    }
}
