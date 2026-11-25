use crate::{
    config::{Profile, Project},
    process,
};
use anyhow::{Context, Result, bail, ensure};
use node_semver::{Range, Version};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct Requirement {
    range: Range,
    text: String,
    source: String,
}

#[derive(Default)]
struct Requirements(Vec<Requirement>);

impl Requirements {
    fn add(&mut self, package: &Value, source: &str) -> Result<()> {
        if let Some(value) = package.pointer("/engines/node") {
            let text = value
                .as_str()
                .with_context(|| format!("{source}: engines.node должен быть строкой"))?;
            if !self.0.iter().any(|r| r.text == text) {
                self.0.push(Requirement {
                    range: Range::parse(text)
                        .with_context(|| format!("{source}: неверный engines.node = {text:?}"))?,
                    text: text.into(),
                    source: source.into(),
                });
            }
        }
        Ok(())
    }

    fn lock(&mut self, lock: &Value, source: &str, platforms: &[(&str, &str)]) -> Result<()> {
        if let Some(packages) = lock.get("packages") {
            for (name, package) in packages
                .as_object()
                .with_context(|| format!("{source}: packages должен быть объектом"))?
            {
                if relevant(package, platforms) {
                    self.add(package, &format!("{source}: {name}"))?;
                }
            }
        } else {
            self.legacy_dependencies(lock, source, platforms)?;
        }
        Ok(())
    }

    fn legacy_dependencies(
        &mut self,
        lock: &Value,
        source: &str,
        platforms: &[(&str, &str)],
    ) -> Result<()> {
        if let Some(dependencies) = lock.get("dependencies").and_then(Value::as_object) {
            for (name, package) in dependencies {
                if relevant(package, platforms) {
                    let source = format!("{source}: {name}");
                    self.add(package, &source)?;
                    self.legacy_dependencies(package, &source, platforms)?;
                }
            }
        }
        Ok(())
    }

    fn read(project: &Project) -> Result<Self> {
        let root = project.root.canonicalize()?;
        let frontend = project.config.frontend.as_ref().unwrap();
        let directory = root.join(&frontend.path).canonicalize()?;
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "x64",
            arch => arch,
        };
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            os => os,
        };
        let mut platforms = vec![(os, arch)];
        if project.config.profile == Profile::Tauri {
            // Linux bundles run on amd64; the Windows cross-build uses a native Linux image.
            platforms.extend([("linux", "x64"), ("linux", arch)]);
        }
        let mut requirements = Self::default();
        let mut found_lock = false;
        for directory in directory.ancestors().take_while(|p| p.starts_with(&root)) {
            let path = directory.join("package.json");
            if let Some(package) = read_json(&path)? {
                requirements.add(&package, &path.strip_prefix(&root)?.display().to_string())?;
            }
            if !found_lock {
                for name in ["npm-shrinkwrap.json", "package-lock.json"] {
                    let path = directory.join(name);
                    if let Some(lock) = read_json(&path)? {
                        requirements.lock(
                            &lock,
                            &path.strip_prefix(&root)?.display().to_string(),
                            &platforms,
                        )?;
                        found_lock = true;
                        break;
                    }
                }
            }
        }
        Ok(requirements)
    }

    fn matches(&self, version: &Version) -> bool {
        self.0.iter().all(|r| r.range.satisfies(version))
    }
}

fn read_json(path: &Path) -> Result<Option<Value>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes)
                .with_context(|| format!("Неверный {}", path.display()))?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("Не удалось прочитать {}", path.display()))
        }
    }
}

fn relevant(package: &Value, platforms: &[(&str, &str)]) -> bool {
    fn supports(values: Option<&Value>, platform: &str) -> bool {
        let Some(values) = values.and_then(Value::as_array) else {
            return true;
        };
        let values: Vec<_> = values.iter().filter_map(Value::as_str).collect();
        !values.iter().any(|v| *v == format!("!{platform}"))
            && (values.iter().all(|v| v.starts_with('!'))
                || values.iter().any(|v| *v == platform || *v == "any"))
    }
    package.get("optional").and_then(Value::as_bool) != Some(true)
        || platforms
            .iter()
            .any(|(os, arch)| supports(package.get("os"), os) && supports(package.get("cpu"), arch))
}

#[derive(Clone, Debug)]
pub struct Runtime {
    pub version: Version,
    bin: PathBuf,
}

impl Runtime {
    fn probe(project: &Project, bin: PathBuf) -> Option<Self> {
        if !bin.join("npm").is_file() || !bin.join("node").is_file() {
            return None;
        }
        let output = process::capture(
            process::command(project, bin.join("node"), &project.root).arg("--version"),
            "Node.js",
        )
        .ok()?;
        let version = Version::parse(output.trim_start_matches('v')).ok()?;
        if version.is_prerelease() || !version.build.is_empty() {
            return None;
        }
        Some(Self { version, bin })
    }

    pub fn apply(&self, command: &mut Command) -> Result<()> {
        process::prepend_path(command, &self.bin)
    }

    pub fn npm(&self, project: &Project, directory: &Path) -> Result<Command> {
        let mut command = process::command(project, self.bin.join("npm"), directory);
        self.apply(&mut command)?;
        Ok(command)
    }
}

fn directories(path: &OsStr, stores: &[(PathBuf, &str)]) -> Vec<PathBuf> {
    let mut directories: Vec<_> = std::env::split_paths(path).collect();
    for (store, suffix) in stores {
        if let Ok(entries) = fs::read_dir(store) {
            let mut versions: Vec<_> = entries.flatten().map(|e| e.path().join(suffix)).collect();
            versions.sort();
            directories.extend(versions);
        }
    }
    let mut seen = BTreeSet::new();
    directories
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

fn installed_directories() -> Vec<PathBuf> {
    let mut stores = Vec::new();
    if let Some(directory) = std::env::var_os("NVM_DIR") {
        stores.push((PathBuf::from(directory).join("versions/node"), "bin"));
    }
    if let Some(directory) = std::env::var_os("FNM_DIR") {
        stores.push((
            PathBuf::from(directory).join("node-versions"),
            "installation/bin",
        ));
    }
    if let Some(directory) = std::env::var_os("XDG_DATA_HOME") {
        stores.push((
            PathBuf::from(directory).join("fnm/node-versions"),
            "installation/bin",
        ));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        stores.extend([
            (home.join(".nvm/versions/node"), "bin"),
            (
                home.join(".local/share/fnm/node-versions"),
                "installation/bin",
            ),
            (
                home.join("Library/Application Support/fnm/node-versions"),
                "installation/bin",
            ),
        ]);
    }
    directories(&std::env::var_os("PATH").unwrap_or_default(), &stores)
}

fn select(
    requirements: &Requirements,
    candidates: &[Runtime],
    current_bin: Option<&Path>,
) -> Result<Runtime> {
    // Keep a compatible active runtime. Otherwise choose the newest compatible installation.
    let current = candidates
        .iter()
        .find(|c| Some(c.bin.as_path()) == current_bin);
    if let Some(current) = current
        && requirements.matches(&current.version)
    {
        return Ok(current.clone());
    }
    if let Some(runtime) = candidates
        .iter()
        .filter(|c| requirements.matches(&c.version))
        .max_by(|a, b| a.version.cmp(&b.version))
    {
        return Ok(runtime.clone());
    }
    let found = candidates
        .iter()
        .map(|c| c.version.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let constraints = requirements
        .0
        .iter()
        .filter(|r| {
            current
                .or_else(|| candidates.first())
                .is_none_or(|c| !r.range.satisfies(&c.version))
        })
        .take(5)
        .map(|r| format!("{}: engines.node = {}", r.source, r.text))
        .collect::<Vec<_>>()
        .join("; ");
    bail!(
        "Не найден совместимый Node.js/npm. Проверены версии: {}. Требования: {}. \
         Установите подходящую версию через nvm/fnm или добавьте её bin в PATH",
        if found.is_empty() { "нет" } else { &found },
        if constraints.is_empty() {
            "не заданы"
        } else {
            &constraints
        },
    )
}

pub fn resolve(project: &Project) -> Result<Option<Runtime>> {
    if project.config.profile != Profile::Tauri
        && !project.config.frontend.as_ref().is_some_and(|f| f.enabled)
    {
        return Ok(None);
    }
    let requirements = Requirements::read(project)?;
    let candidates: Vec<_> = installed_directories()
        .into_iter()
        .filter_map(|bin| Runtime::probe(project, bin))
        .collect();
    let current_bin = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .find(|bin| bin.join("node").is_file())
        .and_then(|bin| bin.canonicalize().ok());
    let runtime = select(&requirements, &candidates, current_bin.as_deref())?;
    let npm = process::capture(
        runtime.npm(project, &project.root)?.arg("--version"),
        "npm выбранного Node.js",
    )?;
    ensure!(!npm.is_empty(), "npm выбранного Node.js не сообщил версию");
    eprintln!(
        "veruna-pack: Node.js v{}, npm {} ({})",
        runtime.version,
        npm,
        runtime.bin.display()
    );
    Ok(Some(runtime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Frontend, test_support::Fixture};
    use serde_json::json;

    fn runtime(version: &str) -> Runtime {
        Runtime {
            version: Version::parse(version).unwrap(),
            bin: PathBuf::from(format!("/node/{version}/bin")),
        }
    }

    fn locked_requirements() -> Requirements {
        let mut requirements = Requirements::default();
        requirements
            .lock(
                &json!({"lockfileVersion": 3, "packages": {
                    "node_modules/vite": {"dev": true, "engines": {"node": "^20.19.0 || >=22.12.0"}},
                    "node_modules/eslint": {"dev": true, "engines": {"node": "^20.19.0 || ^22.13.0 || >=24"}},
                    "node_modules/legacy": {"engines": {"node": ">=8.x"}}
                }}),
                "frontend/package-lock.json",
                &[("darwin", "arm64")],
            )
            .unwrap();
        requirements
    }

    #[test]
    fn lock_engines_allow_node_20_but_not_every_node_22() {
        let requirements = locked_requirements();
        for version in ["20.20.2", "22.13.0", "22.22.0", "24.0.0"] {
            assert!(requirements.matches(&runtime(version).version), "{version}");
        }
        for version in ["18.20.0", "20.18.0", "21.0.0", "22.12.0", "23.5.0"] {
            assert!(
                !requirements.matches(&runtime(version).version),
                "{version}"
            );
        }
    }

    #[test]
    fn selection_keeps_compatible_current_and_respects_package_upper_bound() {
        let mut requirements = locked_requirements();
        let candidates = [
            runtime("20.20.2"),
            runtime("22.12.0"),
            runtime("22.22.0"),
            runtime("24.0.0"),
        ];
        assert_eq!(
            select(&requirements, &candidates, Some(&candidates[0].bin))
                .unwrap()
                .version,
            candidates[0].version
        );
        requirements
            .add(&json!({"engines": {"node": "22.x"}}), "package.json")
            .unwrap();
        assert_eq!(
            select(&requirements, &candidates, Some(&candidates[0].bin))
                .unwrap()
                .version,
            candidates[2].version
        );
    }

    #[test]
    fn missing_node_in_path_selects_newest_compatible_installation() {
        let candidates = [runtime("20.20.2"), runtime("22.22.0"), runtime("24.0.0")];
        assert_eq!(
            select(&locked_requirements(), &candidates, None)
                .unwrap()
                .version,
            candidates[2].version,
        );
    }

    #[test]
    fn requirements_come_from_snapshot_manifests_and_nearest_lock_only() {
        let fixture = Fixture::new("0.1.0", false);
        let mut project = fixture.project();
        let directory = fixture.root.join("frontend");
        fs::create_dir(&directory).unwrap();
        fs::write(
            fixture.temp.path().join("package.json"),
            r#"{"engines":{"node":"<10"}}"#,
        )
        .unwrap();
        fs::write(
            fixture.root.join("package.json"),
            r#"{"engines":{"node":">=18 <22"}}"#,
        )
        .unwrap();
        fs::write(
            fixture.root.join("package-lock.json"),
            r#"{"packages":{"":{"engines":{"node":">=99"}}}}"#,
        )
        .unwrap();
        fs::write(
            directory.join("package.json"),
            r#"{"engines":{"node":">=20"}}"#,
        )
        .unwrap();
        fs::write(
            directory.join("package-lock.json"),
            r#"{"packages":{"node_modules/vite":{"engines":{"node":"^20.19.0 || >=22.12.0"}}}}"#,
        )
        .unwrap();
        project.config.frontend = Some(Frontend {
            path: "frontend".into(),
            enabled: true,
            version_env: "VITE_APP_VERSION".into(),
        });
        let requirements = Requirements::read(&project).unwrap();
        assert!(requirements.matches(&runtime("20.20.2").version));
        assert!(!requirements.matches(&runtime("20.18.0").version));
        assert!(!requirements.matches(&runtime("22.22.0").version));
        fs::write(directory.join("package-lock.json"), "broken json").unwrap();
        assert!(
            format!("{:#}", Requirements::read(&project).err().unwrap())
                .contains("package-lock.json")
        );
    }

    #[test]
    fn optional_packages_are_checked_for_platforms_used_by_the_build() {
        let lock = json!({"packages": {
            "node_modules/linux-only": {"optional": true, "os": ["!darwin"], "cpu": ["x64"], "engines": {"node": ">=24"}},
            "node_modules/arm-only": {"optional": true, "os": ["darwin"], "cpu": ["arm64"], "engines": {"node": ">=20"}}
        }});
        let mut native = Requirements::default();
        native
            .lock(&lock, "package-lock.json", &[("darwin", "arm64")])
            .unwrap();
        assert!(native.matches(&runtime("20.20.2").version));
        assert!(!native.matches(&runtime("18.20.0").version));
        let mut tauri = Requirements::default();
        tauri
            .lock(
                &lock,
                "package-lock.json",
                &[("darwin", "arm64"), ("linux", "x64")],
            )
            .unwrap();
        assert!(!tauri.matches(&runtime("20.20.2").version));
        assert!(tauri.matches(&runtime("24.0.0").version));
    }

    #[test]
    fn legacy_lock_checks_nested_dependencies() {
        let mut requirements = Requirements::default();
        requirements
            .lock(
                &json!({"dependencies": {
                    "tool": {"engines": {"node": ">=18"}, "dependencies": {
                        "nested": {"engines": {"node": "20 || >=22"}}
                    }}
                }}),
                "package-lock.json",
                &[("darwin", "arm64")],
            )
            .unwrap();
        assert!(!requirements.matches(&runtime("18.20.0").version));
        assert!(requirements.matches(&runtime("20.20.2").version));
    }

    #[test]
    fn incompatible_installations_and_invalid_engines_name_the_dependency() {
        let error = select(&locked_requirements(), &[runtime("22.12.0")], None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("node_modules/eslint"));
        assert!(error.contains("22.12.0"));
        assert!(error.contains("^22.13.0"));
        let mut requirements = Requirements::default();
        let error = requirements
            .add(&json!({"engines": {"node": "broken"}}), "package.json")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("package.json: неверный engines.node")
        );
    }

    #[test]
    fn rust_only_and_disabled_frontends_do_not_require_node() {
        let fixture = Fixture::new("0.1.0", false);
        let mut project = fixture.project();
        assert!(resolve(&project).unwrap().is_none());
        project.config.frontend = Some(Frontend {
            path: "absent".into(),
            enabled: false,
            version_env: "VITE_APP_VERSION".into(),
        });
        assert!(resolve(&project).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn installed_nvm_and_fnm_runtime_reaches_npm_and_child_commands() {
        use std::os::unix::fs::PermissionsExt;
        fn install(bin: &Path, version: &str) {
            fs::create_dir_all(bin).unwrap();
            fs::write(
                bin.join("node"),
                format!("#!/bin/sh\nprintf 'v{version}\\n'\n"),
            )
            .unwrap();
            fs::write(bin.join("npm"), "#!/bin/sh\nnode --version\n").unwrap();
            for file in ["node", "npm"] {
                fs::set_permissions(bin.join(file), fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let fixture = Fixture::new("0.1.0", false);
        let project = fixture.project();
        let active = fixture.temp.path().join("old node/bin");
        let nvm = fixture.temp.path().join("nvm/versions/node");
        let fnm = fixture.temp.path().join("fnm/node-versions");
        install(&active, "18.20.0");
        install(&nvm.join("v22.12.0/bin"), "22.12.0");
        install(&fnm.join("v22.22.0/installation/bin"), "22.22.0");
        install(&nvm.join("v24.0.0-rc.1/bin"), "24.0.0-rc.1");
        let path = std::env::join_paths([&active, &active]).unwrap();
        let bins = directories(&path, &[(nvm, "bin"), (fnm, "installation/bin")]);
        assert_eq!(bins.len(), 4);
        let candidates: Vec<_> = bins
            .into_iter()
            .filter_map(|bin| Runtime::probe(&project, bin))
            .collect();
        assert_eq!(candidates.len(), 3);
        let selected = select(&locked_requirements(), &candidates, Some(&active)).unwrap();
        assert_eq!(selected.version, runtime("22.22.0").version);
        let mut command = selected.npm(&project, &project.root).unwrap();
        // Tauri also prepends a Rust toolchain directory after creating the npm command.
        process::prepend_path(&mut command, &active).unwrap();
        selected.apply(&mut command).unwrap();
        let output = process::capture(&mut command, "npm child Node.js").unwrap();
        assert_eq!(output, "v22.22.0");
        let path = command
            .get_envs()
            .find(|(key, _)| *key == "PATH")
            .unwrap()
            .1
            .unwrap();
        assert!(std::env::split_paths(path).any(|p| p == active));
    }
}
