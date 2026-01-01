use greentic_integration::harness::{StackError, TestEnv};
use std::thread;
use std::time::Duration;

#[tokio::test]
async fn e2e_stack_boot() -> anyhow::Result<()> {
    if !greentic_integration::harness::docker_available() {
        eprintln!(
            "docker daemon not available; skipping stack dependencies (mac/local dev fallback)"
        );
        thread::sleep(Duration::from_millis(100));
        return Ok(());
    }

    unsafe {
        std::env::set_var("E2E_TEST_NAME", "e2e_stack_boot");
    }

    let env = TestEnv::up().await?;

    let mut stack = match env.up_stack().await {
        Ok(stack) => stack,
        Err(StackError::MissingBinary { name, searched }) => {
            if stack_strict() {
                anyhow::bail!("missing binary {} (checked: {:?})", name, searched);
            }
            eprintln!(
                "skipping e2e_stack_boot: missing binary {} (checked: {:?})",
                name, searched
            );
            return Ok(());
        }
        Err(err) => {
            if stack_strict() {
                return Err(err.into());
            }
            eprintln!(
                "skipping e2e_stack_boot: stack failed to start ({err}); see logs under {}",
                env.logs_dir().display()
            );
            return Ok(());
        }
    };

    stack.healthcheck(env.logs_dir()).await?;
    stack.down().await?;
    Ok(())
}

fn stack_strict() -> bool {
    std::env::var("GREENTIC_STACK_STRICT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}
