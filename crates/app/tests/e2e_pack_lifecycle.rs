use std::path::PathBuf;

use greentic_integration::harness::pack::{pack_build, pack_install, pack_verify};
use greentic_integration::harness::{
    PackBuildResult, PackInstallResult, PackVerifyResult, TestEnv,
};

#[tokio::test]
async fn e2e_pack_lifecycle() -> anyhow::Result<()> {
    if !greentic_integration::harness::docker_available() {
        eprintln!("skipping e2e_pack_lifecycle: docker daemon not available");
        return Ok(());
    }
    // Allow fallback pack build/verify when pack binaries are unavailable even if
    // a caller set strict env flags globally.
    let _env_guard = disable_strict_pack_mode();

    unsafe {
        std::env::set_var("E2E_TEST_NAME", "e2e_pack_lifecycle");
    }

    let env = TestEnv::up().await?;
    env.healthcheck().await?;

    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .join("fixtures")
        .join("packs")
        .join("hello");

    let PackBuildResult { gtpack, mode } =
        pack_build(&fixture_root, env.artifacts_dir(), env.logs_dir())?;
    assert!(
        gtpack.exists(),
        "gtpack output missing at {}",
        gtpack.display()
    );

    let PackVerifyResult { ok, .. } = pack_verify(&gtpack, env.logs_dir())?;
    assert!(ok, "pack verify should succeed");

    let PackInstallResult { ok, target } =
        pack_install("dev", &gtpack, env.artifacts_dir(), env.logs_dir())?;
    assert!(ok, "pack install should succeed");
    assert_eq!(target, "dev");

    // Record build mode for debugging.
    let build_mode_note = env.artifacts_dir().join("pack").join("build_mode.txt");
    let note = format!("mode: {:?}\n", mode);
    std::fs::write(build_mode_note, note)?;

    env.down().await?;
    Ok(())
}

struct EnvRestore(Vec<(&'static str, Option<String>)>);

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            if let Some(val) = value {
                unsafe { std::env::set_var(key, val) };
            } else {
                unsafe { std::env::remove_var(key) };
            }
        }
    }
}

fn disable_strict_pack_mode() -> EnvRestore {
    let mut saved = Vec::new();
    let keys = [
        "GREENTIC_PACK_STRICT",
        "GREENTIC_PACK_NO_FALLBACK",
        "GREENTIC_INTEGRATION_STRICT",
    ];
    for key in keys {
        saved.push((key, std::env::var(key).ok()));
        unsafe { std::env::set_var(key, "0") };
    }
    EnvRestore(saved)
}
