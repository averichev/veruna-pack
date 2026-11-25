use crate::{
    artifacts,
    config::{Profile, Project, Target},
    node::{self, Runtime},
    plan::Plan,
    process,
    signing::Material,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub fn run(
    project: &Project,
    plan: &Plan,
    output: &Path,
    cache: &Path,
    material: &Material,
) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "Полный выпуск требует сборочный Mac с Apple SDK"
    );
    open_file_limit()?;
    let node = node::resolve(project)?;
    preflight(project, output.parent().unwrap())?;
    let version = plan.version.as_ref().unwrap().to_string();
    if let Some(frontend) = project.config.frontend.as_ref().filter(|f| f.enabled) {
        let directory = project.root.join(&frontend.path);
        let node = node.as_ref().unwrap();
        process::run(
            node.npm(project, &directory)?.arg("ci"),
            "npm ci (frontend)",
        )?;
        process::run(
            node.npm(project, &directory)?
                .args(["run", "build"])
                .env(&frontend.version_env, &version)
                .env("VERUNA_RELEASE_VERSION", &version),
            "сборка frontend",
        )?;
    }
    fs::create_dir_all(cache)?;
    match project.config.profile {
        Profile::Standalone => standalone(project, &version, output, cache, node.as_ref()),
        Profile::Tauri => tauri(
            project,
            plan,
            output,
            cache,
            material,
            node.as_ref().unwrap(),
        ),
    }
}

fn check(project: &Project, tool: &str, args: &[&str]) -> Result<()> {
    process::capture(
        process::command(project, tool, &project.root).args(args),
        tool,
    )?;
    Ok(())
}

fn open_file_limit() -> Result<()> {
    #[cfg(unix)]
    {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // getrlimit/setrlimit receive a valid live rlimit; only this child CLI's
        // soft limit changes. The terminal and the hard limit are unaffected.
        ensure!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0,
            "getrlimit: {}",
            std::io::Error::last_os_error()
        );
        let wanted = 8192.min(limit.rlim_max);
        if limit.rlim_cur < wanted {
            limit.rlim_cur = wanted;
            ensure!(
                unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } == 0,
                "setrlimit: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    Ok(())
}

fn preflight(project: &Project, work: &Path) -> Result<()> {
    check(project, "xcrun", &["--sdk", "macosx", "--show-sdk-path"])?;
    let toolchain = match project.config.profile {
        Profile::Standalone => &project.config.rust.as_ref().unwrap().toolchain,
        Profile::Tauri => &project.config.tauri.as_ref().unwrap().toolchain,
    };
    let rustc = process::tool_path(project, toolchain, "rustc")?;
    let probe = work.join("target-probe.rs");
    fs::write(
        &probe,
        "pub fn probe() -> usize { std::mem::size_of::<usize>() }\n",
    )?;
    for target in &project.config.targets {
        if project.config.profile == Profile::Tauri && target.os != "macos" {
            continue;
        }
        let targets = if target.arch == "universal" {
            vec!["aarch64-apple-darwin", "x86_64-apple-darwin"]
        } else {
            vec![target.rust(&project.config.profile)]
        };
        for triple in targets {
            process::capture(process::command(project, &rustc, &project.root).args(["--crate-type=lib", "--emit=metadata", "--target", triple, "--out-dir"]).arg(work).arg(&probe), "проверка стандартной библиотеки Rust").with_context(|| format!("Установите target в выбранный toolchain: rustup target add --toolchain {toolchain} {triple}"))?;
        }
    }
    match project.config.profile {
        Profile::Standalone => {
            check(project, "zig", &["version"])?;
            check(project, "cargo-zigbuild", &["--version"])?;
            check(project, "x86_64-w64-mingw32-gcc", &["--version"])?;
            check(project, "x86_64-w64-mingw32-ar", &["--version"])?;
        }
        Profile::Tauri => {
            check(project, "7zz", &["i"])?;
            check(project, "docker", &["info", "--format", "{{.OSType}}"])?;
            check(project, "docker", &["buildx", "version"])?;
        }
    }
    Ok(())
}

fn standalone(
    project: &Project,
    version: &str,
    output: &Path,
    cache: &Path,
    node: Option<&Runtime>,
) -> Result<()> {
    let settings = project.config.rust.as_ref().unwrap();
    let app = project.root.join(&project.config.app);
    let rustc = process::tool_path(project, &settings.toolchain, "rustc")?;
    for target in &project.config.targets {
        let triple = target.rust(&Profile::Standalone);
        let target_dir = cache.join("standalone");
        let mut command = process::cargo(project, &app, &settings.toolchain);
        if let Some(node) = node {
            node.apply(&mut command)?;
        }
        command
            .args([
                if target.os == "linux" {
                    "zigbuild"
                } else {
                    "build"
                },
                "--locked",
                "--release",
                "--bin",
                &settings.binary,
                "--target",
                triple,
                "--target-dir",
            ])
            .arg(&target_dir)
            .env("RUSTC", &rustc)
            .env("CARGO_BUILD_RUSTC", &rustc)
            .env(&settings.version_env, version)
            .env("VERUNA_RELEASE_VERSION", version);
        if !settings.features.is_empty() {
            command.arg("--features").arg(settings.features.join(","));
        }
        if target.os == "windows" {
            command
                .env("CC_x86_64_pc_windows_gnu", "x86_64-w64-mingw32-gcc")
                .env("AR_x86_64_pc_windows_gnu", "x86_64-w64-mingw32-ar")
                .env(
                    "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER",
                    "x86_64-w64-mingw32-gcc",
                );
        }
        process::run(&mut command, &format!("Cargo {triple}"))?;
        let binary = target_dir.join(triple).join("release").join(format!(
            "{}{}",
            settings.binary,
            if target.os == "windows" { ".exe" } else { "" }
        ));
        let name = format!(
            "{}{}",
            artifacts::stem(project, version, target),
            match target.os.as_str() {
                "windows" => ".exe",
                "macos" => ".tool",
                _ => "",
            }
        );
        fs::copy(binary, output.join(name))?;
    }
    Ok(())
}

fn tauri(
    project: &Project,
    plan: &Plan,
    output: &Path,
    cache: &Path,
    material: &Material,
    node: &Runtime,
) -> Result<()> {
    let Material::Tauri { encoded, password } = material else {
        unreachable!()
    };
    let version = plan.version.as_ref().unwrap().to_string();
    let app = project.root.join(&project.config.app);
    let settings = project.config.tauri.as_ref().unwrap();
    let cargo = process::tool_path(project, &settings.toolchain, "cargo")?;
    let rustc = process::tool_path(project, &settings.toolchain, "rustc")?;
    for target in &project.config.targets {
        let bundles = match target.os.as_str() {
            "macos" => vec!["app", "dmg"],
            "linux" => vec!["deb"],
            "windows" => vec!["nsis"],
            _ => unreachable!(),
        };
        let override_name = format!(".veruna-pack-{}.json", target.os);
        artifacts::write_json(
            &app.join(&override_name),
            &json!({
                "version": version,
                "identifier": project.identifier(),
                "plugins": { "updater": { "pubkey": project.identity["plugins"]["updater"]["pubkey"] } },
                "build": { "beforeBuildCommand": "" },
                "bundle": { "active": true, "createUpdaterArtifacts": true, "targets": bundles },
            }),
        )?;
        let bundle_root = if target.os == "macos" {
            let target_dir = cache.join("tauri-macos");
            let bundle_root = target_dir
                .join(target.rust(&Profile::Tauri))
                .join("release/bundle");
            if bundle_root.exists() {
                fs::remove_dir_all(&bundle_root)?;
            }
            let mut command = node.npm(project, &app)?;
            command.args(["exec", "--", "tauri", "build", "--ci", "--runner", &cargo]);
            if let Some(path) = &settings.release_config {
                command.arg("--config").arg(path);
            }
            command.args([
                "--config",
                &override_name,
                "--target",
                target.rust(&Profile::Tauri),
                "--bundles",
                "app,dmg",
                "--",
                "--locked",
            ]);
            command
                .env("TAURI_SIGNING_PRIVATE_KEY", encoded.as_str())
                .env("TAURI_SIGNING_PRIVATE_KEY_PASSWORD", password.as_str())
                .env("RUSTC", &rustc)
                .env("CARGO_BUILD_RUSTC", &rustc)
                .env("CARGO_TARGET_DIR", &target_dir)
                .env("VITE_APP_VERSION", &version);
            process::prepend_path(&mut command, Path::new(&cargo).parent().unwrap())?;
            node.apply(&mut command)?;
            process::run(&mut command, "Tauri macOS")?;
            bundle_root
        } else {
            docker_tauri(
                project,
                &version,
                target,
                output.parent().unwrap(),
                &override_name,
                material,
                node,
            )?
        };
        collect_tauri(project, &version, target, &bundle_root, output)?;
    }
    Ok(())
}

fn docker_tauri(
    project: &Project,
    version: &str,
    target: &Target,
    work: &Path,
    override_name: &str,
    material: &Material,
    node: &Runtime,
) -> Result<PathBuf> {
    let Material::Tauri { encoded, password } = material else {
        unreachable!()
    };
    let dockerfile = work.join("tauri.Dockerfile");
    fs::write(
        &dockerfile,
        include_str!("../build-support/tauri.Dockerfile"),
    )?;
    // This is an isolated Git snapshot. Host node_modules and caches never enter images.
    fs::write(
        project.root.join(".dockerignore"),
        ".git\n**/node_modules\n**/target\nreleases\n",
    )?;
    let mut key = tempfile::NamedTempFile::new()?;
    key.write_all(encoded.as_bytes())?;
    let mut pass = tempfile::NamedTempFile::new()?;
    pass.write_all(password.as_bytes())?;
    let export = work.join(format!("docker-{}", target.os));
    let mut command = process::command(project, "docker", &project.root);
    command
        .args(["buildx", "build", "--progress", "plain", "--file"])
        .arg(&dockerfile)
        .arg("--target")
        .arg(format!("{}-artifacts", target.os));
    if target.os == "linux" {
        command.args(["--platform", "linux/amd64"]);
    }
    for (key, value) in [
        ("APP_DIR", project.config.app.to_string_lossy().into_owned()),
        ("APP_VERSION", version.into()),
        ("NODE_VERSION", node.version.to_string()),
        (
            "RUST_VERSION",
            project
                .config
                .tauri
                .as_ref()
                .unwrap()
                .docker_rust_version
                .clone(),
        ),
        (
            "RELEASE_CONFIG",
            project
                .config
                .tauri
                .as_ref()
                .unwrap()
                .release_config
                .as_deref()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .into_owned(),
        ),
        ("OVERRIDE_CONFIG", override_name.into()),
    ] {
        command.arg("--build-arg").arg(format!("{key}={value}"));
    }
    command
        .arg("--secret")
        .arg(format!(
            "id=tauri_signing_private_key,src={}",
            key.path().display()
        ))
        .arg("--secret")
        .arg(format!(
            "id=tauri_signing_password,src={}",
            pass.path().display()
        ));
    command
        .arg("--output")
        .arg(format!("type=local,dest={}", export.display()))
        .arg(".");
    process::run(&mut command, &format!("Tauri Docker {}", target.os))?;
    Ok(export.join("bundle"))
}

fn single(directory: &Path, suffix: &str) -> Result<PathBuf> {
    let files: Vec<_> = fs::read_dir(directory)
        .with_context(|| format!("Не найден каталог пакетов {}", directory.display()))?
        .map(|e| Ok(e?.path()))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(suffix))
        })
        .collect();
    ensure!(
        files.len() == 1,
        "Требуется ровно один *{suffix} в {}, найдено {}",
        directory.display(),
        files.len()
    );
    artifacts::regular(&files[0])?;
    ensure!(fs::metadata(&files[0])?.len() > 0, "Пустой пакет");
    Ok(files[0].clone())
}

pub fn collect_tauri(
    project: &Project,
    version: &str,
    target: &Target,
    bundle: &Path,
    output: &Path,
) -> Result<()> {
    let stem = artifacts::stem(project, version, target);
    let (folder, extension, destination) = match target.os.as_str() {
        "macos" => ("dmg", ".dmg", format!("{stem}.dmg")),
        "linux" => ("deb", ".deb", format!("{stem}.deb")),
        "windows" => ("nsis", ".exe", format!("{stem}-setup.exe")),
        _ => unreachable!(),
    };
    let installer = single(&bundle.join(folder), extension)?;
    ensure!(
        installer
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(version),
        "Имя пакета не содержит рассчитанную версию"
    );
    fs::copy(&installer, output.join(&destination))?;
    let (updater, name) = if target.os == "macos" {
        (
            single(&bundle.join("macos"), ".app.tar.gz")?,
            format!("{stem}.app.tar.gz"),
        )
    } else {
        (installer, destination)
    };
    let signature = PathBuf::from(format!("{}.sig", updater.display()));
    artifacts::regular(&signature)
        .context("Tauri не создала фактический пакет обновления с .sig")?;
    if target.os == "macos" {
        fs::copy(updater, output.join(&name))?;
    }
    fs::copy(signature, output.join(format!("{name}.sig")))?;
    Ok(())
}
