use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use greentic_integration_core::errors::CoreError;
use greentic_integration_core::model::SubstitutionContext;
use greentic_integration_core::substitute::substitute;
use serde::Serialize;

use crate::gtest::{CommandLine, Directive, Scenario, StepKind};
use crate::json::{assert, diff, normalize};

const STDOUT_LIMIT: usize = 1024 * 1024;
const STDERR_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub workdir: Option<PathBuf>,
    pub keep_workdir: bool,
    pub repo_root: PathBuf,
    pub prepend_path: Option<String>,
    pub artifacts_dir: Option<PathBuf>,
    pub seed: Option<u64>,
    pub normalize_config: normalize::NormalizeConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenarioStatus {
    Passed,
    Failed,
}

#[derive(Debug, Clone)]
pub struct ScenarioResult {
    pub name: String,
    pub path: PathBuf,
    pub status: ScenarioStatus,
    pub start_ms: u64,
    pub end_ms: u64,
    pub failure: Option<ScenarioFailure>,
    pub replay_hint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScenarioFailure {
    pub line_no: usize,
    pub message: String,
}

#[derive(Debug, Clone)]
struct LastRun {
    line_no: usize,
    command: String,
    argv: Vec<String>,
    exit: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
struct ExecutionContext {
    cwd: PathBuf,
    test_root: PathBuf,
    artifacts_root: Option<PathBuf>,
    artifacts_dir: Option<PathBuf>,
    env_overrides: HashMap<String, String>,
    substitution: SubstitutionContext,
    last_run: Option<LastRun>,
    last_run_exit_checked: bool,
    normalize_config: normalize::NormalizeConfig,
}

#[derive(Debug, Serialize)]
struct StepMeta {
    step: usize,
    line_no: usize,
    command: String,
    exit_code: Option<i32>,
    duration_ms: u128,
}

#[derive(Debug, Serialize)]
struct ScenarioMeta {
    name: String,
    start_ms: u64,
    end_ms: u64,
    seed: Option<u64>,
}

pub fn run_scenarios(scenarios: Vec<Scenario>, options: RunOptions) -> Result<Vec<ScenarioResult>> {
    let multi = scenarios.len() > 1;
    let mut results = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        let test_root = scenario
            .path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| options.repo_root.clone());
        let (workdir, keep_workdir) = prepare_workdir(
            options.workdir.as_ref(),
            scenario.name.as_str(),
            options.keep_workdir,
        )?;
        let scenario_root = options.artifacts_dir.as_ref().map(|base| {
            if multi {
                base.join(sanitize_name(&scenario.name))
            } else {
                base.clone()
            }
        });
        let scenario_artifacts = scenario_root.as_ref().map(|dir| dir.join("artifacts"));
        if let Some(dir) = &scenario_root {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create artifacts dir {}", dir.display()))?;
        }
        if let Some(dir) = &scenario_artifacts {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create artifacts dir {}", dir.display()))?;
        }
        let result = run_scenario(
            &scenario,
            &ScenarioOptions {
                workdir,
                keep_workdir,
                test_root,
                repo_root: options.repo_root.clone(),
                prepend_path: options.prepend_path.clone(),
                artifacts_root: scenario_root,
                artifacts_dir: scenario_artifacts,
                seed: options.seed,
                normalize_config: options.normalize_config.clone(),
            },
        )?;
        results.push(result);
    }
    Ok(results)
}

struct ScenarioOptions {
    workdir: PathBuf,
    keep_workdir: bool,
    test_root: PathBuf,
    repo_root: PathBuf,
    prepend_path: Option<String>,
    artifacts_root: Option<PathBuf>,
    artifacts_dir: Option<PathBuf>,
    seed: Option<u64>,
    normalize_config: normalize::NormalizeConfig,
}

fn run_scenario(scenario: &Scenario, options: &ScenarioOptions) -> Result<ScenarioResult> {
    let start_ms = now_ms();
    let tmp_dir = options.workdir.join("tmp");
    std::fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("failed to create tmp dir {}", tmp_dir.display()))?;

    let mut ctx = ExecutionContext {
        cwd: options.workdir.clone(),
        test_root: options.test_root.clone(),
        artifacts_root: options.artifacts_root.clone(),
        artifacts_dir: options.artifacts_dir.clone(),
        env_overrides: HashMap::new(),
        substitution: SubstitutionContext::default(),
        last_run: None,
        last_run_exit_checked: false,
        normalize_config: options.normalize_config.clone(),
    };
    if let Some(seed) = options.seed {
        ctx.env_overrides
            .insert("GREENTIC_FAIL_SEED".to_string(), seed.to_string());
    }

    ctx.substitution.builtin.insert(
        "TEST_DIR".to_string(),
        ctx.test_root.to_string_lossy().into_owned(),
    );
    ctx.substitution.builtin.insert(
        "WORK_DIR".to_string(),
        options.workdir.to_string_lossy().into_owned(),
    );
    ctx.substitution.builtin.insert(
        "REPO_ROOT".to_string(),
        options.repo_root.to_string_lossy().into_owned(),
    );
    ctx.substitution.builtin.insert(
        "TMP_DIR".to_string(),
        tmp_dir.to_string_lossy().into_owned(),
    );
    if let Some(dir) = options.artifacts_dir.as_ref() {
        ctx.substitution.builtin.insert(
            "ARTIFACTS_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        );
    }

    let mut failure: Option<ScenarioFailure> = None;
    let mut replay_hint: Option<String> = None;
    let mut step_index = 0usize;

    for step in &scenario.steps {
        if failure.is_some() {
            break;
        }
        match &step.kind {
            StepKind::Command(command) => {
                if let Some(last_run) = &ctx.last_run
                    && last_run.exit.unwrap_or(0) != 0
                    && !ctx.last_run_exit_checked
                {
                    failure = Some(ScenarioFailure {
                        line_no: last_run.line_no,
                        message: format!(
                            "unexpected exit code {:?} for '{}'",
                            last_run.exit, last_run.command
                        ),
                    });
                    break;
                }
                match execute_command(step.line_no, command, options, &ctx) {
                    Ok(run) => {
                        step_index += 1;
                        if let Some(dir) = options.artifacts_dir.as_ref() {
                            write_step_artifacts(dir, step_index, step.line_no, &run)?;
                        }
                        ctx.last_run = Some(LastRun {
                            line_no: step.line_no,
                            command: run.command.clone(),
                            argv: run.argv.clone(),
                            exit: run.exit,
                            stdout: run.stdout.clone(),
                            stderr: run.stderr.clone(),
                        });
                        ctx.last_run_exit_checked = false;
                    }
                    Err(err) => {
                        failure = Some(ScenarioFailure {
                            line_no: step.line_no,
                            message: format!("{err}"),
                        });
                        break;
                    }
                }
            }
            StepKind::Directive(directive) => {
                if let Err(err) = apply_directive(step.line_no, directive, &mut ctx) {
                    failure = Some(ScenarioFailure {
                        line_no: err.line_no(),
                        message: err.to_string(),
                    });
                    break;
                }
            }
        }
    }

    if failure.is_none()
        && let Some(last_run) = &ctx.last_run
        && last_run.exit.unwrap_or(0) != 0
        && !ctx.last_run_exit_checked
    {
        failure = Some(ScenarioFailure {
            line_no: last_run.line_no,
            message: format!(
                "unexpected exit code {:?} for '{}'",
                last_run.exit, last_run.command
            ),
        });
    }

    if failure.is_some() {
        replay_hint = build_replay_hint(&ctx);
    }

    let end_ms = now_ms();
    if let Some(dir) = options.artifacts_root.as_ref() {
        let meta = ScenarioMeta {
            name: scenario.name.clone(),
            start_ms,
            end_ms,
            seed: options.seed,
        };
        let payload = serde_json::to_string_pretty(&meta)?;
        let path = dir.join("scenario.meta.json");
        std::fs::write(&path, payload)
            .with_context(|| format!("failed to write scenario meta {}", path.display()))?;
    }

    let status = if failure.is_some() {
        ScenarioStatus::Failed
    } else {
        ScenarioStatus::Passed
    };

    if status == ScenarioStatus::Passed && !options.keep_workdir {
        let _ = std::fs::remove_dir_all(&options.workdir);
    }

    Ok(ScenarioResult {
        name: scenario.name.clone(),
        path: scenario.path.clone(),
        status,
        start_ms,
        end_ms,
        failure,
        replay_hint,
    })
}

struct CommandRun {
    command: String,
    argv: Vec<String>,
    exit: Option<i32>,
    stdout: String,
    stderr: String,
    duration_ms: u128,
}

fn execute_command(
    line_no: usize,
    command: &CommandLine,
    options: &ScenarioOptions,
    ctx: &ExecutionContext,
) -> Result<CommandRun> {
    let mut argv = Vec::with_capacity(command.argv.len());
    for token in &command.argv {
        argv.push(substitute(token, &ctx.substitution, line_no)?);
    }
    let mut cmd = Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    cmd.current_dir(&ctx.cwd);
    let mut envs: HashMap<OsString, OsString> = std::env::vars_os().collect();
    for (key, value) in &ctx.env_overrides {
        envs.insert(OsString::from(key), OsString::from(value));
    }
    if let Some(prepend) = &options.prepend_path {
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
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn command '{}' at line {line_no}", argv[0]))?;

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
    let duration_ms = start.elapsed().as_millis();

    Ok(CommandRun {
        command: argv.join(" "),
        argv,
        exit,
        stdout,
        stderr,
        duration_ms,
    })
}

fn apply_directive(
    line_no: usize,
    directive: &Directive,
    ctx: &mut ExecutionContext,
) -> Result<(), DirectiveError> {
    match directive {
        Directive::Set { key, value } => {
            let value =
                substitute(value, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            ctx.substitution.test_vars.insert(key.clone(), value);
        }
        Directive::Env { key, value } => {
            let value =
                substitute(value, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            ctx.substitution.env_vars.insert(key.clone(), value.clone());
            ctx.env_overrides.insert(key.clone(), value);
        }
        Directive::CaptureStdout { path } => {
            let path =
                substitute(path, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let target = resolve_path(ctx, &path);
            let Some(last_run) = ctx.last_run.as_ref() else {
                return Err(DirectiveError::message(
                    line_no,
                    "no command output available for #CAPTURE_STDOUT".to_string(),
                ));
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|err| DirectiveError::new(line_no, err))?;
            }
            std::fs::write(&target, &last_run.stdout)
                .map_err(|err| DirectiveError::new(line_no, err))?;
        }
        Directive::CaptureJson { path } => {
            let path =
                substitute(path, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let target = resolve_path(ctx, &path);
            let Some(last_run) = ctx.last_run.as_ref() else {
                return Err(DirectiveError::message(
                    line_no,
                    "no command output available for #CAPTURE_JSON".to_string(),
                ));
            };
            let value: serde_json::Value =
                serde_json::from_str(&last_run.stdout).map_err(|_| {
                    DirectiveError::message(line_no, "stdout is not valid JSON".to_string())
                })?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|err| DirectiveError::new(line_no, err))?;
            }
            let payload = serde_json::to_string_pretty(&value)
                .map_err(|err| DirectiveError::message(line_no, err.to_string()))?;
            std::fs::write(&target, payload).map_err(|err| DirectiveError::new(line_no, err))?;
        }
        Directive::Workdir { path } => {
            let path =
                substitute(path, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let next = if Path::new(&path).is_absolute() {
                PathBuf::from(&path)
            } else {
                ctx.test_root.join(&path)
            };
            ctx.cwd = next;
        }
        Directive::Mkdir { path } => {
            let path =
                substitute(path, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let target = if Path::new(&path).is_absolute() {
                PathBuf::from(path)
            } else {
                ctx.cwd.join(path)
            };
            std::fs::create_dir_all(&target).map_err(|err| DirectiveError::new(line_no, err))?;
        }
        Directive::Write { path, content } => {
            let path =
                substitute(path, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let target = resolve_path(ctx, &path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|err| DirectiveError::new(line_no, err))?;
            }
            std::fs::write(&target, content).map_err(|err| DirectiveError::new(line_no, err))?;
        }
        Directive::NormalizeJson { input, output } => {
            let input =
                substitute(input, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let output =
                substitute(output, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let input_path = resolve_path(ctx, &input);
            let output_path = resolve_path(ctx, &output);
            let raw = std::fs::read_to_string(&input_path)
                .map_err(|err| DirectiveError::new(line_no, err))?;
            let mut value: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|_| DirectiveError::message(line_no, "invalid JSON".to_string()))?;
            normalize::normalize_value(&mut value, &ctx.normalize_config);
            let payload = serde_json::to_string_pretty(&value)
                .map_err(|err| DirectiveError::message(line_no, err.to_string()))?;
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| DirectiveError::new(line_no, err))?;
            }
            std::fs::write(&output_path, payload)
                .map_err(|err| DirectiveError::new(line_no, err))?;
        }
        Directive::DiffJson { left, right } => {
            let left =
                substitute(left, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let right =
                substitute(right, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let left_path = resolve_path(ctx, &left);
            let right_path = resolve_path(ctx, &right);
            let left_raw = std::fs::read_to_string(&left_path)
                .map_err(|err| DirectiveError::new(line_no, err))?;
            let right_raw = std::fs::read_to_string(&right_path)
                .map_err(|err| DirectiveError::new(line_no, err))?;
            let left_value: serde_json::Value = serde_json::from_str(&left_raw)
                .map_err(|_| DirectiveError::message(line_no, "invalid JSON".to_string()))?;
            let right_value: serde_json::Value = serde_json::from_str(&right_raw)
                .map_err(|_| DirectiveError::message(line_no, "invalid JSON".to_string()))?;
            if let Some(diff) = diff::diff_values(&left_value, &right_value) {
                return Err(DirectiveError::message(line_no, diff));
            }
        }
        Directive::SaveArtifact { path } => {
            let path =
                substitute(path, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let source = resolve_path(ctx, &path);
            let artifacts_dir = ctx.artifacts_dir.as_ref().ok_or_else(|| {
                DirectiveError::message(
                    line_no,
                    "artifacts dir not configured; pass --artifacts-dir".to_string(),
                )
            })?;
            if !source.exists() {
                return Err(DirectiveError::message(
                    line_no,
                    format!("artifact source not found: {}", source.display()),
                ));
            }
            let file_name = source.file_name().ok_or_else(|| {
                DirectiveError::message(line_no, "invalid artifact path".to_string())
            })?;
            let target = artifacts_dir.join(file_name);
            std::fs::copy(&source, &target).map_err(|err| DirectiveError::new(line_no, err))?;
        }
        Directive::TrySaveTrace { path } => {
            let path =
                substitute(path, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let source = resolve_path(ctx, &path);
            let artifacts_dir = ctx.artifacts_dir.as_ref().ok_or_else(|| {
                DirectiveError::message(
                    line_no,
                    "artifacts dir not configured; pass --artifacts-dir".to_string(),
                )
            })?;
            if source.exists() {
                let target = artifacts_dir.join("trace.json");
                std::fs::copy(&source, &target).map_err(|err| DirectiveError::new(line_no, err))?;
            }
        }
        Directive::FailDropStateWrite => {
            ctx.env_overrides.insert(
                "GREENTIC_FAIL_DROP_STATE_WRITE".to_string(),
                "1".to_string(),
            );
        }
        Directive::FailDelayStateRead { ms } => {
            let ms = substitute(ms, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let delay: u64 = ms.parse().map_err(|_| {
                DirectiveError::message(line_no, format!("invalid delay value '{ms}'"))
            })?;
            ctx.env_overrides.insert(
                "GREENTIC_FAIL_DELAY_STATE_READ_MS".to_string(),
                delay.to_string(),
            );
        }
        Directive::FailAssetTransient { ratio } => {
            let ratio =
                substitute(ratio, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            validate_ratio(line_no, &ratio)?;
            ctx.env_overrides
                .insert("GREENTIC_FAIL_ASSET_TRANSIENT".to_string(), ratio);
        }
        Directive::FailDuplicateInteraction => {
            ctx.env_overrides.insert(
                "GREENTIC_FAIL_DUPLICATE_INTERACTION".to_string(),
                "1".to_string(),
            );
        }
        Directive::ExpectJsonPath {
            file,
            path,
            op,
            value,
        } => {
            let file =
                substitute(file, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let path =
                substitute(path, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let op_str =
                substitute(op, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let value = value
                .as_ref()
                .map(|val| substitute(val, &ctx.substitution, line_no))
                .transpose()
                .map_err(DirectiveError::from)?;
            let op = assert::JsonPathOp::parse(&op_str)
                .map_err(|err| DirectiveError::message(line_no, err.to_string()))?;
            let path_buf = resolve_path(ctx, &file);
            let raw = std::fs::read_to_string(&path_buf)
                .map_err(|err| DirectiveError::new(line_no, err))?;
            let value_json: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|_| DirectiveError::message(line_no, "invalid JSON".to_string()))?;
            assert::evaluate_jsonpath(&value_json, &path, op, value.as_deref())
                .map_err(|err| DirectiveError::message(line_no, err.to_string()))?;
        }
        Directive::ExpectExit { code } => {
            let code =
                substitute(code, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let expected: i32 = code.parse().map_err(|_| {
                DirectiveError::message(line_no, format!("invalid exit code '{code}'"))
            })?;
            let Some(last_run) = ctx.last_run.as_ref() else {
                return Err(DirectiveError::message(
                    line_no,
                    "no command output available for #EXPECT_EXIT".to_string(),
                ));
            };
            if last_run.exit != Some(expected) {
                return Err(DirectiveError::message(
                    line_no,
                    format!("expected exit {expected}, got {:?}", last_run.exit),
                ));
            }
            ctx.last_run_exit_checked = true;
        }
        Directive::ExpectStdoutContains { value } => {
            let value =
                substitute(value, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let Some(last_run) = ctx.last_run.as_ref() else {
                return Err(DirectiveError::message(
                    line_no,
                    "no command output available for #EXPECT_STDOUT_CONTAINS".to_string(),
                ));
            };
            if !last_run.stdout.contains(&value) {
                return Err(DirectiveError::message(
                    line_no,
                    format!("stdout missing '{value}'"),
                ));
            }
        }
        Directive::ExpectStderrContains { value } => {
            let value =
                substitute(value, &ctx.substitution, line_no).map_err(DirectiveError::from)?;
            let Some(last_run) = ctx.last_run.as_ref() else {
                return Err(DirectiveError::message(
                    line_no,
                    "no command output available for #EXPECT_STDERR_CONTAINS".to_string(),
                ));
            };
            if !last_run.stderr.contains(&value) {
                return Err(DirectiveError::message(
                    line_no,
                    format!("stderr missing '{value}'"),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct DirectiveError {
    line_no: usize,
    message: String,
}

impl DirectiveError {
    fn new(line_no: usize, err: std::io::Error) -> Self {
        Self {
            line_no,
            message: err.to_string(),
        }
    }

    fn message(line_no: usize, message: String) -> Self {
        Self { line_no, message }
    }

    fn line_no(&self) -> usize {
        self.line_no
    }
}

impl std::fmt::Display for DirectiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl From<CoreError> for DirectiveError {
    fn from(err: CoreError) -> Self {
        match err {
            CoreError::ParseError { line_no, message }
            | CoreError::TokenizeError { line_no, message } => Self { line_no, message },
            CoreError::MissingVar { line_no, var } => Self {
                line_no,
                message: format!("missing variable '{var}'"),
            },
        }
    }
}

fn write_step_artifacts(
    dir: &Path,
    step_index: usize,
    line_no: usize,
    run: &CommandRun,
) -> Result<()> {
    let step = format!("step-{:03}", step_index);
    let stdout_path = dir.join(format!("{step}.stdout.log"));
    let stderr_path = dir.join(format!("{step}.stderr.log"));
    std::fs::write(&stdout_path, &run.stdout)
        .with_context(|| format!("failed to write {}", stdout_path.display()))?;
    std::fs::write(&stderr_path, &run.stderr)
        .with_context(|| format!("failed to write {}", stderr_path.display()))?;
    let meta = StepMeta {
        step: step_index,
        line_no,
        command: run.command.clone(),
        exit_code: run.exit,
        duration_ms: run.duration_ms,
    };
    let payload = serde_json::to_string_pretty(&meta)?;
    let meta_path = dir.join(format!("{step}.meta.json"));
    std::fs::write(&meta_path, payload)
        .with_context(|| format!("failed to write {}", meta_path.display()))?;
    Ok(())
}

fn build_replay_hint(ctx: &ExecutionContext) -> Option<String> {
    let Some(artifacts_dir) = ctx.artifacts_dir.as_ref() else {
        return Some("No trace.json found; enable runner tracing with --trace-out.".to_string());
    };
    let trace_path = artifacts_dir.join("trace.json");
    if !trace_path.exists()
        && let Some(found) = discover_trace_path(ctx)
    {
        let _ = std::fs::copy(&found, &trace_path);
    }
    if trace_path.exists() {
        return Some(format!(
            "Replay: greentic-runner replay {}",
            trace_path.to_string_lossy()
        ));
    }
    Some("No trace.json found; enable runner tracing with --trace-out.".to_string())
}

fn discover_trace_path(ctx: &ExecutionContext) -> Option<PathBuf> {
    if let Some(value) = ctx.env_overrides.get("GREENTIC_TRACE_OUT") {
        let path = PathBuf::from(value);
        if path.exists() {
            return Some(path);
        }
    }
    if let Ok(value) = std::env::var("GREENTIC_TRACE_OUT") {
        let path = PathBuf::from(value);
        if path.exists() {
            return Some(path);
        }
    }
    if let Some(last_run) = ctx.last_run.as_ref()
        && let Some(path) = parse_trace_out_arg(&last_run.argv)
        && path.exists()
    {
        return Some(path);
    }
    if let Some(root) = ctx.artifacts_root.as_ref() {
        let candidates = [
            root.join("trace.json"),
            root.join("artifacts").join("trace.json"),
        ];
        for path in candidates {
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

fn parse_trace_out_arg(argv: &[String]) -> Option<PathBuf> {
    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        if arg == "--trace-out" {
            if let Some(value) = iter.next() {
                return Some(PathBuf::from(value));
            }
        } else if let Some(value) = arg.strip_prefix("--trace-out=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

fn validate_ratio(line_no: usize, value: &str) -> Result<(), DirectiveError> {
    let mut iter = value.splitn(2, '/');
    let Some(n_str) = iter.next() else {
        return Err(DirectiveError::message(
            line_no,
            format!("invalid ratio '{value}'"),
        ));
    };
    let Some(m_str) = iter.next() else {
        return Err(DirectiveError::message(
            line_no,
            format!("invalid ratio '{value}'"),
        ));
    };
    let n: u64 = n_str
        .parse()
        .map_err(|_| DirectiveError::message(line_no, format!("invalid ratio '{value}'")))?;
    let m: u64 = m_str
        .parse()
        .map_err(|_| DirectiveError::message(line_no, format!("invalid ratio '{value}'")))?;
    if n == 0 || m == 0 || n > m {
        return Err(DirectiveError::message(
            line_no,
            format!("invalid ratio '{value}'"),
        ));
    }
    Ok(())
}

fn prepare_workdir(
    base: Option<&PathBuf>,
    scenario_name: &str,
    keep_workdir: bool,
) -> Result<(PathBuf, bool)> {
    if let Some(base) = base {
        let name = sanitize_name(scenario_name);
        let dir = base.join(name);
        std::fs::create_dir_all(&dir)?;
        return Ok((dir, keep_workdir));
    }
    let dir = tempfile::Builder::new()
        .prefix("gtest-")
        .tempdir()
        .context("failed to create temp workdir")?;
    let path = dir.keep();
    Ok((path, keep_workdir))
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

fn resolve_path(ctx: &ExecutionContext, path: &str) -> PathBuf {
    if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        ctx.cwd.join(path)
    }
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
    use crate::gtest::Directive;

    #[test]
    fn set_directive_substitutes_values() {
        let mut ctx = ExecutionContext {
            cwd: PathBuf::from("."),
            test_root: PathBuf::from("."),
            artifacts_root: None,
            artifacts_dir: None,
            env_overrides: HashMap::new(),
            substitution: SubstitutionContext::default(),
            last_run: None,
            last_run_exit_checked: false,
            normalize_config: normalize::NormalizeConfig::default(),
        };
        ctx.substitution
            .test_vars
            .insert("BASE".to_string(), "value".to_string());
        let directive = Directive::Set {
            key: "OUT".to_string(),
            value: "${BASE}-next".to_string(),
        };
        apply_directive(1, &directive, &mut ctx).unwrap();
        assert_eq!(
            ctx.substitution.test_vars.get("OUT").map(String::as_str),
            Some("value-next")
        );
    }
}
