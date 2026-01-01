use std::process::Command;

use anyhow::Result;

/// Regression harness: runs key E2E scenarios (PR-13–PR-17) sequentially and fails fast.
#[test]
fn e2e_regression_suite() -> Result<()> {
    if std::env::var("E2E_REGRESSION_CHILD").is_ok() {
        // Avoid recursion if invoked by itself.
        return Ok(());
    }
    let tests = [
        "pr13_greentic_dev_e2e",
        "e2e_greentic_dev_negative",
        "e2e_greentic_dev_offline",
        "e2e_greentic_dev_snapshot",
        "e2e_greentic_dev_multi_pack",
        "pr14_provider_core_flows_and_index",
        "pr14_provider_core_schema_onboarding",
    ];
    for name in tests {
        let status = Command::new("cargo")
            .args([
                "test",
                "-p",
                "greentic-integration",
                name,
                "--",
                "--nocapture",
            ])
            .env("E2E_REGRESSION_CHILD", "1")
            .status()?;
        if !status.success() {
            anyhow::bail!(
                "regression test {name} failed with status {:?}",
                status.code()
            );
        }
    }
    Ok(())
}
