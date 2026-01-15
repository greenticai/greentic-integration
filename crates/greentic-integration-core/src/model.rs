//! Data model for .gtest scripts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A parsed test plan with ordered steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestPlan {
    pub path: PathBuf,
    pub steps: Vec<Step>,
}

/// A single line in a test plan with preserved raw content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub line_no: usize,
    pub raw: String,
    pub kind: StepKind,
}

/// The kind of step represented by a line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepKind {
    Directive(Directive),
    Command(CommandLine),
}

/// Supported directives for .gtest scripts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Directive {
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
        duration: Duration,
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

/// A command line parsed into argv tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandLine {
    pub argv: Vec<String>,
}

/// Context used to substitute variables in commands and directives.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubstitutionContext {
    pub test_vars: HashMap<String, String>,
    pub env_vars: HashMap<String, String>,
    pub builtin: HashMap<String, String>,
}
