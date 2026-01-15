//! Errors for parsing and substitution.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("parse error on line {line_no}: {message}")]
    ParseError { line_no: usize, message: String },
    #[error("missing variable '{var}' on line {line_no}")]
    MissingVar { line_no: usize, var: String },
    #[error("tokenize error on line {line_no}: {message}")]
    TokenizeError { line_no: usize, message: String },
}
