use crate::{
    config::{Profile, Project, Target, stable_version},
    inspect,
    plan::Plan,
    signing,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, io::Read, path::Path};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub os: String,
    pub protocol_os: String,
    pub arch: String,
    pub update_targets: Vec<String>,
    pub roles: Vec<String>,
    pub name: String,
    pub signature: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    #[serde(flatten)]
    pub spec: Spec,
    pub size_bytes: u64,
    pub sha256: String,
}
impl std::ops::Deref for Artifact {
    type Target = Spec;
    fn deref(&self) -> &Spec {
        &self.spec
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u8,
    pub application: String,
    pub identifier: String,
    pub profile: Profile,
    pub version: String,
    pub cli_version: String,
    pub source_commit: String,
    pub previous_tag: Option<String>,
    pub tag: String,
    pub config_sha256: String,
    pub artifacts: Vec<Artifact>,
}

pub fn stem(project: &Project, version: &str, target: &Target) -> String {
    format!(
        "{}-{version}-{}-{}",
        project.config.slug, target.os, target.arch
    )
}

pub fn specs(project: &Project, plan: &Plan) -> Vec<Spec> {
    let version = plan.version.as_ref().unwrap().to_string();
    let mut result = Vec::new();
    for target in &project.config.targets {
        let name = stem(project, &version, target);
        let mut add = |name: String, roles: &[&str], signed: bool| {
            let signature = signed.then(|| {
                format!(
                    "{name}.{}",
                    if project.config.profile == Profile::Standalone {
                        "jws"
                    } else {
                        "sig"
                    }
                )
            });
            let spec = Spec {
                os: target.os.clone(),
                protocol_os: target.protocol_os().into(),
                arch: target.arch.clone(),
                update_targets: target.update_targets(),
                roles: roles.iter().map(|s| (*s).into()).collect(),
                name,
                signature: signature.clone(),
            };
            result.push(spec.clone());
            if let Some(name) = signature {
                result.push(Spec {
                    name,
                    roles: vec!["signature".into()],
                    signature: None,
                    ..spec
                });
            }
        };
        match (&project.config.profile, target.os.as_str()) {
            (Profile::Standalone, os) => add(
                format!(
                    "{name}{}",
                    match os {
                        "windows" => ".exe",
                        "macos" => ".tool",
                        _ => "",
                    }
                ),
                &["executable", "updater"],
                true,
            ),
            (Profile::Tauri, "macos") => {
                add(format!("{name}.dmg"), &["installer"], false);
                add(format!("{name}.app.tar.gz"), &["updater"], true);
            }
            (Profile::Tauri, "linux") => {
                add(format!("{name}.deb"), &["installer", "updater"], true)
            }
            (Profile::Tauri, "windows") => {
                add(format!("{name}-setup.exe"), &["installer", "updater"], true)
            }
            _ => unreachable!(),
        }
    }
    result
}

pub fn regular(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)
            .with_context(|| format!("Отсутствует {}", path.display()))?
            .file_type()
            .is_file(),
        "Ожидается обычный файл: {}",
        path.display()
    );
    Ok(())
}
pub fn digest(path: &Path) -> Result<(u64, String)> {
    regular(path)?;
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buffer = [0; 1024 * 64];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hash.update(&buffer[..n]);
    }
    Ok((size, format!("{:x}", hash.finalize())))
}
pub fn record(directory: &Path, spec: Spec) -> Result<Artifact> {
    let (size_bytes, sha256) = digest(&directory.join(&spec.name))?;
    ensure!(size_bytes > 0, "Пустой файл: {}", spec.name);
    Ok(Artifact {
        spec,
        size_bytes,
        sha256,
    })
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(())
}

pub fn finish(
    project: &Project,
    plan: &Plan,
    directory: &Path,
    material: &signing::Material,
) -> Result<()> {
    let specs = specs(project, plan);
    if let signing::Material::Standalone(key) = material {
        for spec in specs.iter().filter(|s| s.signature.is_some()) {
            let artifact = record(directory, spec.clone())?;
            fs::write(
                directory.join(spec.signature.as_ref().unwrap()),
                signing::sign_jws(
                    project,
                    &plan.version.as_ref().unwrap().to_string(),
                    &artifact,
                    key,
                )?,
            )?;
        }
    }
    let artifacts: Vec<_> = specs
        .into_iter()
        .map(|spec| record(directory, spec))
        .collect::<Result<_>>()?;
    let manifest = Manifest {
        schema_version: 1,
        application: project.config.slug.clone(),
        identifier: project.identifier().into(),
        profile: project.config.profile.clone(),
        version: plan.version.as_ref().unwrap().to_string(),
        cli_version: env!("CARGO_PKG_VERSION").into(),
        source_commit: plan.source_commit.clone(),
        previous_tag: plan.previous_tag.clone(),
        tag: plan.tag.clone().unwrap(),
        config_sha256: project.fingerprint.clone(),
        artifacts,
    };
    fs::write(directory.join("CHANGELOG.md"), &plan.changelog)?;
    write_json(&directory.join("release.json"), &manifest)?;
    fs::write(directory.join("SHA256SUMS"), sums(directory, plan)?)?;
    Ok(())
}

fn sums(directory: &Path, plan: &Plan) -> Result<String> {
    let mut names = plan.files.clone();
    names.sort();
    let mut sums = String::new();
    for name in names.iter().filter(|n| *n != "SHA256SUMS") {
        sums.push_str(&format!("{}  {name}\n", digest(&directory.join(name))?.1));
    }
    Ok(sums)
}

fn metadata_differences(expected: serde_json::Value, actual: serde_json::Value) -> String {
    expected
        .as_object()
        .expect("metadata is an object")
        .iter()
        .filter_map(|(field, expected)| {
            let actual = &actual[field];
            (expected != actual)
                .then(|| format!("  {field}: ожидается {expected}; в выпуске {actual}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn verify(project: &Project, plan: &Plan, directory: &Path) -> Result<()> {
    verify_using(project, plan, directory, inspect::verify)
}

pub fn verify_using(
    project: &Project,
    plan: &Plan,
    directory: &Path,
    inspect: impl Fn(&Project, &Plan, &Path, &Spec) -> Result<()>,
) -> Result<()> {
    ensure!(
        fs::symlink_metadata(directory)?.file_type().is_dir(),
        "Ожидается каталог выпуска без symlink: {}",
        directory.display()
    );
    let absolute = directory.canonicalize()?;
    let directory = absolute.as_path();
    let names: BTreeSet<_> = fs::read_dir(directory)?
        .map(|entry| {
            entry?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("Имя не UTF-8"))
        })
        .collect::<Result<_>>()?;
    let expected_names: BTreeSet<_> = plan.files.iter().cloned().collect();
    if names != expected_names {
        let missing: Vec<_> = expected_names.difference(&names).cloned().collect();
        let extra: Vec<_> = names.difference(&expected_names).cloned().collect();
        let list = |names: &[String]| {
            if names.is_empty() {
                "нет".into()
            } else {
                format!("\n  {}", names.join("\n  "))
            }
        };
        let legacy_hint = if names.contains("build.json") && !names.contains("release.json") {
            "\nНайден build.json без release.json: это не полный комплект VerunaPack."
        } else {
            ""
        };
        anyhow::bail!(
            "Неверный состав выпуска {}\nОтсутствуют файлы ({}): {}\nЛишние файлы ({}): {}{legacy_hint}",
            directory.display(),
            missing.len(),
            list(&missing),
            extra.len(),
            list(&extra)
        );
    }
    for name in &names {
        regular(&directory.join(name))?;
    }
    ensure!(
        fs::read_to_string(directory.join("CHANGELOG.md"))? == plan.changelog,
        "CHANGELOG.md не соответствует диапазону Git {}..{}",
        plan.previous_tag.as_deref().unwrap_or("начало истории"),
        plan.source_commit
    );
    let manifest_path = directory.join("release.json");
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(&manifest_path)?).with_context(|| {
            format!(
                "Не удалось прочитать метаданные {}",
                manifest_path.display()
            )
        })?;
    let differences = metadata_differences(
        serde_json::json!({
            "schema_version": 1,
            "application": project.config.slug,
            "identifier": project.identifier(),
            "profile": project.config.profile,
            "version": plan.version.as_ref().map(ToString::to_string),
            "source_commit": plan.source_commit,
            "previous_tag": plan.previous_tag,
            "tag": plan.tag,
            "config_sha256": project.fingerprint,
        }),
        serde_json::to_value(&manifest)?,
    );
    ensure!(
        differences.is_empty(),
        "release.json не соответствует плану:\n{differences}"
    );
    stable_version(&manifest.cli_version).context("Некорректная cli_version в release.json")?;
    let expected = specs(project, plan);
    ensure!(
        manifest.artifacts.len() == expected.len(),
        "Неверное число артефактов в release.json: ожидается {}, в выпуске {}",
        expected.len(),
        manifest.artifacts.len()
    );
    for (artifact, spec) in manifest.artifacts.iter().zip(expected) {
        let differences = metadata_differences(
            serde_json::to_value(&spec)?,
            serde_json::to_value(&artifact.spec)?,
        );
        ensure!(
            differences.is_empty(),
            "Метаданные артефакта в release.json не соответствуют плану: {}\n{differences}",
            spec.name
        );
        let actual = record(directory, spec.clone())?;
        ensure!(
            *artifact == actual,
            "Повреждён файл {}\n  Размер: ожидается {} байт; фактически {} байт\n  SHA-256: ожидается {}; фактически {}",
            spec.name,
            artifact.size_bytes,
            actual.size_bytes,
            artifact.sha256,
            actual.sha256
        );
        if let Some(signature) = &spec.signature {
            let signature_path = directory.join(signature);
            let signature = fs::read_to_string(&signature_path)
                .with_context(|| format!("Не удалось прочитать {}", signature_path.display()))?;
            match project.config.profile {
                Profile::Standalone => {
                    signing::verify_jws(project, &manifest.version, artifact, &signature)
                }
                Profile::Tauri => {
                    signing::verify_tauri(project, &directory.join(&spec.name), &signature)
                }
            }
            .with_context(|| format!("Проверка подписи {}", signature_path.display()))?;
        }
        if spec.roles != ["signature"] {
            inspect(project, plan, directory, &spec)
                .with_context(|| format!("Проверка {}", spec.name))?;
        }
    }
    ensure!(
        fs::read_to_string(directory.join("SHA256SUMS"))? == sums(directory, plan)?,
        "SHA256SUMS повреждён или не покрывает все файлы"
    );
    Ok(())
}
