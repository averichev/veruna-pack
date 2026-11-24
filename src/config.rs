use crate::git;
use anyhow::{Context, Result, bail, ensure};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    Standalone,
    Tauri,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub os: String,
    pub arch: String,
}

impl Target {
    pub fn rust(&self, profile: &Profile) -> &str {
        match (profile, self.os.as_str(), self.arch.as_str()) {
            (Profile::Standalone, "linux", "x86_64") => "x86_64-unknown-linux-musl",
            (Profile::Standalone, "windows", "x86_64") => "x86_64-pc-windows-gnu",
            (Profile::Tauri, "linux", "x86_64") => "x86_64-unknown-linux-gnu",
            (Profile::Tauri, "windows", "x86_64") => "x86_64-pc-windows-msvc",
            (_, "macos", "aarch64") => "aarch64-apple-darwin",
            (_, "macos", "x86_64") => "x86_64-apple-darwin",
            (Profile::Tauri, "macos", "universal") => "universal-apple-darwin",
            _ => unreachable!("configuration validates targets"),
        }
    }
    pub fn protocol_os(&self) -> &str {
        if self.os == "macos" {
            "darwin"
        } else {
            &self.os
        }
    }
    pub fn update_targets(&self) -> Vec<String> {
        if self.arch == "universal" {
            vec!["darwin-aarch64".into(), "darwin-x86_64".into()]
        } else {
            vec![format!("{}-{}", self.protocol_os(), self.arch)]
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub slug: String,
    pub profile: Profile,
    #[serde(default = "dot")]
    pub app: PathBuf,
    pub targets: Vec<Target>,
    #[serde(default)]
    pub exclude: Vec<PathBuf>,
    #[serde(default)]
    pub push_tag: bool,
    pub frontend: Option<Frontend>,
    pub rust: Option<Rust>,
    pub tauri: Option<Tauri>,
    pub signing: Signing,
}

fn dot() -> PathBuf {
    PathBuf::from(".")
}
fn stable() -> String {
    "stable".into()
}
fn vite() -> String {
    "VITE_APP_VERSION".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Frontend {
    pub path: PathBuf,
    #[serde(default = "vite")]
    pub version_env: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rust {
    pub binary: String,
    pub version_env: String,
    #[serde(default = "stable")]
    pub toolchain: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default = "version_args")]
    pub version_args: Vec<String>,
}
fn version_args() -> Vec<String> {
    vec!["--version".into()]
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tauri {
    pub release_config: PathBuf,
    #[serde(default = "stable")]
    pub toolchain: String,
    #[serde(default = "docker_rust")]
    pub docker_rust_version: String,
}
fn docker_rust() -> String {
    "1.91.1".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Signing {
    /// Standalone identity JSON: product_id and updates.public_key.
    pub identity: Option<PathBuf>,
    pub private_key_env: String,
    pub private_key_file: Option<PathBuf>,
    pub password_env: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Project {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub config: Config,
    pub identity: Value,
    pub base_version: Version,
    pub fingerprint: String,
}

pub fn relative(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty() && !path.is_absolute(),
        "Ожидается относительный путь: {}",
        path.display()
    );
    ensure!(
        path.components()
            .all(|p| matches!(p, Component::Normal(_) | Component::CurDir)),
        "Путь выходит за пределы проекта: {}",
        path.display()
    );
    Ok(())
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn env_name(value: &str) -> bool {
    token(value) && !value.starts_with(|c: char| c.is_ascii_digit()) && !value.contains('-')
}
fn toolchain(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl Config {
    fn validate(&self) -> Result<()> {
        ensure!(
            token(&self.slug) && self.slug == self.slug.to_ascii_lowercase(),
            "slug: используйте строчные ASCII-буквы, цифры, '_' и '-'"
        );
        relative(&self.app)?;
        for p in &self.exclude {
            relative(p)?;
            ensure!(
                p.components().any(|c| matches!(c, Component::Normal(_))),
                "Нельзя исключить весь проект"
            );
        }
        if let Some(f) = &self.frontend {
            relative(&f.path)?;
            ensure!(
                env_name(&f.version_env),
                "Неверное имя frontend.version_env"
            );
        }
        ensure!(
            env_name(&self.signing.private_key_env),
            "Неверное имя signing.private_key_env"
        );
        if let Some(p) = &self.signing.password_env {
            ensure!(env_name(p), "Неверное имя signing.password_env");
        }
        ensure!(
            self.targets.len() == 3,
            "Полный выпуск требует ровно три цели: linux, windows, macos"
        );
        let mut platforms = BTreeSet::new();
        for t in &self.targets {
            ensure!(
                platforms.insert(t.os.as_str()),
                "Повтор платформы: {}",
                t.os
            );
            let supported = match t.os.as_str() {
                "linux" | "windows" => t.arch == "x86_64",
                "macos" => {
                    ["aarch64", "x86_64"].contains(&t.arch.as_str())
                        || self.profile == Profile::Tauri && t.arch == "universal"
                }
                _ => false,
            };
            ensure!(supported, "Неподдерживаемая цель: {}-{}", t.os, t.arch);
        }
        match self.profile {
            Profile::Standalone => {
                let r = self
                    .rust
                    .as_ref()
                    .context("Для standalone требуется [rust]")?;
                ensure!(self.tauri.is_none(), "[tauri] несовместим с standalone");
                ensure!(
                    token(&r.binary) && env_name(&r.version_env) && toolchain(&r.toolchain),
                    "Неверные параметры [rust]"
                );
                relative(
                    self.signing
                        .identity
                        .as_deref()
                        .context("Для standalone требуется signing.identity")?,
                )?;
            }
            Profile::Tauri => {
                let t = self.tauri.as_ref().context("Для tauri требуется [tauri]")?;
                ensure!(self.rust.is_none(), "[rust] несовместим с tauri");
                ensure!(
                    self.signing.identity.is_none(),
                    "Tauri читает закреплённый ключ из src-tauri/tauri.conf.json"
                );
                relative(&t.release_config)?;
                ensure!(toolchain(&t.toolchain), "Неверный tauri.toolchain");
                stable_version(&t.docker_rust_version)?;
                ensure!(
                    self.frontend.as_ref().is_some_and(|f| f.path == self.app),
                    "Для Tauri frontend.path должен совпадать с app"
                );
            }
        }
        Ok(())
    }
}

pub fn stable_version(value: &str) -> Result<Version> {
    let version = Version::parse(value).context("Ожидается стабильная SemVer X.Y.Z")?;
    ensure!(
        version.pre.is_empty() && version.build.is_empty() && version.to_string() == value,
        "Ожидается каноническая стабильная версия X.Y.Z: {value}"
    );
    Ok(version)
}

impl Project {
    pub fn load(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .with_context(|| format!("Не найден {}", path.display()))?;
        let root = PathBuf::from(git::read(
            path.parent().unwrap(),
            &["rev-parse", "--show-toplevel"],
        )?);
        let config_path = path.strip_prefix(&root)?.to_owned();
        Self::from_root(root, config_path)
    }

    pub fn from_root(root: PathBuf, config_path: PathBuf) -> Result<Self> {
        let bytes = fs::read(root.join(&config_path))?;
        let config: Config =
            toml::from_str(std::str::from_utf8(&bytes)?).context("Неверный release.toml")?;
        config.validate()?;
        for path in std::iter::once(&config.app)
            .chain(config.frontend.iter().map(|f| &f.path))
            .chain(config.signing.identity.iter())
        {
            ensure!(
                root.join(path)
                    .canonicalize()?
                    .starts_with(root.canonicalize()?),
                "Путь через symlink выходит за пределы Git-репозитория: {}",
                path.display()
            );
        }
        let (identity, base_version) = match config.profile {
            Profile::Standalone => {
                let identity: Value = serde_json::from_slice(&fs::read(
                    root.join(config.signing.identity.as_ref().unwrap()),
                )?)?;
                let id = identity["product_id"]
                    .as_str()
                    .context("identity.product_id отсутствует")?;
                ensure!(
                    uuid::Uuid::parse_str(id)?.to_string() == id,
                    "product_id должен быть каноническим UUID"
                );
                (identity, cargo_version(&root, &config.app)?)
            }
            Profile::Tauri => {
                let identity: Value = serde_json::from_slice(&fs::read(
                    root.join(&config.app).join("src-tauri/tauri.conf.json"),
                )?)?;
                ensure!(
                    identity["identifier"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty()),
                    "Нет Tauri identifier"
                );
                ensure!(
                    identity["plugins"]["updater"]["pubkey"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty()),
                    "Нет закреплённого Tauri updater.pubkey"
                );
                let version =
                    stable_version(identity["version"].as_str().context("Нет Tauri version")?)?;
                let release: Value = serde_json::from_slice(&fs::read(
                    root.join(&config.app)
                        .join(&config.tauri.as_ref().unwrap().release_config),
                )?)?;
                if release.get("identifier").is_some()
                    || release.pointer("/plugins/updater/pubkey").is_some()
                {
                    bail!("release_config не должен менять identifier или закреплённый ключ");
                }
                (identity, version)
            }
        };
        let mut hash = Sha256::new();
        hash.update(&bytes);
        hash.update(serde_json::to_vec(&identity)?);
        let fingerprint = format!("{:x}", hash.finalize());
        Ok(Self {
            root,
            config_path,
            config,
            identity,
            base_version,
            fingerprint,
        })
    }

    pub fn excluded(&self, path: &Path) -> bool {
        self.config.exclude.iter().any(|p| path.starts_with(p))
    }

    pub fn identifier(&self) -> &str {
        match self.config.profile {
            Profile::Standalone => self.identity["product_id"].as_str().unwrap(),
            Profile::Tauri => self.identity["identifier"].as_str().unwrap(),
        }
    }
}

fn cargo_version(root: &Path, app: &Path) -> Result<Version> {
    let app = root.join(app).canonicalize()?;
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(app.join("Cargo.toml"))?)?;
    if let Some(version) = manifest
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
    {
        return stable_version(version);
    }
    ensure!(
        manifest
            .get("package")
            .and_then(|p| p.get("version"))
            .and_then(|v| v.get("workspace"))
            .and_then(|v| v.as_bool())
            == Some(true),
        "Нет версии приложения в Cargo.toml"
    );
    for directory in app.ancestors().take_while(|p| p.starts_with(root)) {
        let path = directory.join("Cargo.toml");
        if !path.is_file() {
            continue;
        }
        let manifest: toml::Value = toml::from_str(&fs::read_to_string(path)?)?;
        if let Some(version) = manifest
            .get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_str())
        {
            return stable_version(version);
        }
    }
    bail!("Не найден workspace.package.version внутри Git-репозитория")
}
