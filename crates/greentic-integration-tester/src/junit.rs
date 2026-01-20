use std::path::Path;

use anyhow::{Context, Result};

use crate::gtest::{ScenarioResult, ScenarioStatus};

pub fn write_junit(path: &Path, suite_name: &str, results: &[ScenarioResult]) -> Result<()> {
    let tests = results.len();
    let failures = results
        .iter()
        .filter(|result| result.status == ScenarioStatus::Failed)
        .count();
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(&format!(
        "<testsuite name=\"{}\" tests=\"{}\" failures=\"{}\">\n",
        escape_xml(suite_name),
        tests,
        failures
    ));
    for result in results {
        let duration = (result.end_ms.saturating_sub(result.start_ms)) as f64 / 1000.0;
        xml.push_str(&format!(
            "  <testcase name=\"{}\" classname=\"{}\" time=\"{:.3}\">",
            escape_xml(&result.name),
            escape_xml(&result.path.to_string_lossy()),
            duration
        ));
        if let Some(failure) = &result.failure {
            let mut message = failure.message.clone();
            if let Some(hint) = &result.replay_hint {
                message.push_str(" | ");
                message.push_str(hint);
            }
            let message = escape_xml(&message);
            xml.push_str(&format!(
                "<failure message=\"{}\">line {}: {}</failure>",
                message, failure.line_no, message
            ));
        }
        xml.push_str("</testcase>\n");
    }
    xml.push_str("</testsuite>\n");
    std::fs::write(path, xml)
        .with_context(|| format!("failed to write junit report {}", path.display()))?;
    Ok(())
}

fn escape_xml(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gtest::ScenarioStatus;
    use crate::gtest::executor::ScenarioFailure;
    use tempfile::NamedTempFile;

    #[test]
    fn junit_includes_failure() {
        let results = vec![ScenarioResult {
            name: "demo".to_string(),
            path: "tests/demo.gtest".into(),
            status: ScenarioStatus::Failed,
            start_ms: 0,
            end_ms: 1000,
            failure: Some(ScenarioFailure {
                line_no: 4,
                message: "boom".to_string(),
            }),
            replay_hint: Some("Replay: greentic-runner replay /tmp/trace.json".to_string()),
        }];
        let file = NamedTempFile::new().unwrap();
        write_junit(file.path(), "suite", &results).unwrap();
        let xml = std::fs::read_to_string(file.path()).unwrap();
        assert!(xml.contains("<failure"));
        assert!(xml.contains("Replay: greentic-runner replay"));
    }
}
