use crate::config::Project;
use anyhow::{Context, Result, ensure};
use std::{
    path::Path,
    process::{Command, Stdio},
};

pub fn command(project: &Project, program: impl AsRef<std::ffi::OsStr>, cwd: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(cwd).stdin(Stdio::null());
    for name in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_TARGET_DIR",
        "RUSTC",
        "CARGO_BUILD_RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "TAURI_SIGNING_PRIVATE_KEY",
        "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
        "MOCKPORT_SIGNING_KEY",
        "MOCKPORT_KEY_PASSPHRASE",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
    ] {
        cmd.env_remove(name);
    }
    cmd.env_remove(&project.config.signing.private_key_env);
    if let Some(name) = &project.config.signing.password_env {
        cmd.env_remove(name);
    }
    if let Some(rust) = &project.config.rust {
        cmd.env_remove(&rust.version_env);
    }
    cmd
}

pub fn run(cmd: &mut Command, label: &str) -> Result<()> {
    eprintln!("veruna-pack: {label}");
    let status = cmd
        .status()
        .with_context(|| format!("Не удалось запустить {label}"))?;
    ensure!(status.success(), "Ошибка {label}: {status}");
    Ok(())
}

pub fn capture(cmd: &mut Command, label: &str) -> Result<String> {
    let output = cmd
        .output()
        .with_context(|| format!("Не удалось запустить {label}"))?;
    ensure!(
        output.status.success(),
        "Ошибка {label}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

pub fn cargo(project: &Project, cwd: &Path, toolchain: &str) -> Command {
    let mut cmd = command(project, "rustup", cwd);
    cmd.args(["run", toolchain, "cargo"]);
    cmd
}

pub fn tool_path(project: &Project, toolchain: &str, tool: &str) -> Result<String> {
    capture(
        command(project, "rustup", &project.root).args(["which", "--toolchain", toolchain, tool]),
        "определение Rust toolchain",
    )
}
