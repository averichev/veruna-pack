use crate::{
    config::{Config, Profile},
    selection::{Choice, Selection},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

fn manifest(path: &Path) -> Result<toml::Value> {
    toml::from_str(
        &fs::read_to_string(path)
            .with_context(|| format!("Не удалось прочитать {}", path.display()))?,
    )
    .with_context(|| format!("Неверный {}", path.display()))
}

fn children(root: &Path, parent: &str) -> Result<Vec<PathBuf>> {
    let path = root.join(parent);
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            result.push(Path::new(parent).join(entry.file_name()));
        }
    }
    Ok(result)
}

fn binaries(app: &Path, cargo: &toml::Value) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    let mut explicit_paths = BTreeSet::new();
    if let Some(bins) = cargo.get("bin").and_then(toml::Value::as_array) {
        for bin in bins {
            if let Some(name) = bin.get("name").and_then(toml::Value::as_str) {
                names.insert(name.to_owned());
            }
            if let Some(path) = bin.get("path").and_then(toml::Value::as_str) {
                explicit_paths.insert(app.join(path));
            }
        }
    }
    if package_value(cargo, "autobins").and_then(toml::Value::as_bool) != Some(false) {
        let main = app.join("src/main.rs");
        if main.is_file()
            && !explicit_paths.contains(&main)
            && let Some(name) = package_value(cargo, "name").and_then(toml::Value::as_str)
        {
            names.insert(name.to_owned());
        }
        let directory = app.join("src/bin");
        if directory.is_dir() {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let path = entry.path();
                let file_type = entry.file_type()?;
                let name = if file_type.is_file()
                    && path.extension().is_some_and(|e| e == "rs")
                    && !explicit_paths.contains(&path)
                {
                    path.file_stem()
                } else if file_type.is_dir()
                    && path.join("main.rs").is_file()
                    && !explicit_paths.contains(&path.join("main.rs"))
                {
                    path.file_name()
                } else {
                    continue;
                };
                names.insert(
                    name.context("Нет имени Cargo binary")?
                        .to_str()
                        .context("Имя binary должно быть UTF-8")?
                        .to_owned(),
                );
            }
        }
    }
    Ok(names)
}

fn package_value<'a>(cargo: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    cargo.get("package")?.get(key)
}

fn in_app(app: &Path, path: &str) -> PathBuf {
    if app == Path::new(".") {
        PathBuf::from(path)
    } else {
        app.join(path)
    }
}

fn is_standalone(root: &Path, directory: &Path) -> Result<bool> {
    let app = root.join(directory);
    let cargo = app.join("Cargo.toml");
    if !cargo.is_file()
        || (directory.ends_with("src-tauri") && app.join("tauri.conf.json").is_file())
    {
        return Ok(false);
    }
    Ok(!binaries(&app, &manifest(&cargo)?)?.is_empty())
}

fn standalone_identity(root: &Path, app: &Path) -> PathBuf {
    let release = in_app(app, "release.config.json");
    let updates = in_app(app, "update-identity.json");
    if !root.join(&release).exists() && root.join(&updates).is_file() {
        updates
    } else {
        release
    }
}

fn select_app(
    root: &Path,
    discovery_dir: &Path,
    profile: Option<&Profile>,
    exclude: &[PathBuf],
    selection: &mut Selection,
) -> Result<PathBuf> {
    // Conventional application locations; custom layouts use a single `app` override.
    let mut directories = vec![PathBuf::from(".")];
    directories.extend(children(root, "")?);
    for container in ["apps", "crates", "packages"] {
        directories.extend(children(root, container)?);
    }
    directories.sort();
    directories.dedup();
    directories.retain(|path| !exclude.iter().any(|excluded| path.starts_with(excluded)));
    let tauri: BTreeSet<&Path> = directories
        .iter()
        .map(PathBuf::as_path)
        .filter(|p| {
            profile != Some(&Profile::Standalone)
                && root.join(p).join("src-tauri/tauri.conf.json").is_file()
        })
        .collect();
    ensure!(
        profile != Some(&Profile::Tauri) || !tauri.is_empty(),
        "Tauri-приложение не найдено. Укажите app в release.toml"
    );
    let is_app =
        |path: &Path| -> Result<bool> {
            Ok(tauri.contains(path)
                || (profile != Some(&Profile::Tauri) && is_standalone(root, path)?))
        };
    if !discovery_dir.as_os_str().is_empty() {
        for ancestor in discovery_dir.ancestors() {
            let path = if ancestor.as_os_str().is_empty() {
                Path::new(".")
            } else {
                ancestor
            };
            if directories.iter().any(|dir| dir == path) && is_app(path)? {
                return Ok(path.to_owned());
            }
        }
    }
    if tauri.is_empty() && is_standalone(root, Path::new("."))? {
        return Ok(PathBuf::from("."));
    }
    let mut apps = Vec::new();
    for directory in &directories {
        if is_app(directory)? {
            apps.push(directory.clone());
        }
    }
    let kind = if tauri.is_empty() {
        "Rust"
    } else if tauri.len() == apps.len() {
        "Tauri"
    } else {
        "Rust/Tauri"
    };
    selection.choose(Choice::Application(kind), apps)
}

fn merge(base: &mut Value, overrides: Value) {
    if let (Some(base), Value::Object(overrides)) = (base.as_object_mut(), &overrides) {
        for (key, value) in overrides {
            merge(base.entry(key).or_insert(Value::Null), value.clone());
        }
    } else {
        *base = overrides;
    }
}

fn frontend(root: &Path, app: &Path, overrides: &Value, tauri: bool) -> Result<Value> {
    let candidates = if tauri {
        vec![app.to_owned()]
    } else {
        vec![in_app(app, "frontend"), app.to_owned()]
    };
    let path = if let Some(value) = overrides.pointer("/frontend/path") {
        Some(serde_json::from_value::<PathBuf>(value.clone())?)
    } else {
        let mut found = None;
        for path in candidates {
            let package = root.join(&path).join("package.json");
            if package.is_file() {
                let package: Value = serde_json::from_slice(&fs::read(package)?)?;
                if package
                    .pointer("/scripts/build")
                    .is_some_and(Value::is_string)
                {
                    found = Some(path);
                    break;
                }
            }
        }
        found.or_else(|| {
            overrides
                .get("frontend")
                .filter(|v| v.is_object())
                .map(|_| app.to_owned())
        })
    };
    Ok(match path {
        Some(path) => json!({"path": path, "version_env": "VITE_APP_VERSION", "enabled": true}),
        None => Value::Null,
    })
}

pub fn resolve(
    root: &Path,
    discovery_dir: &Path,
    overrides: Value,
    selection: &mut Selection,
) -> Result<Config> {
    let profile: Option<Profile> = overrides
        .get("profile")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()?;
    let exclude: Vec<PathBuf> = overrides
        .get("exclude")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()?
        .unwrap_or_default();
    for path in &exclude {
        crate::config::relative(path)?;
    }
    let app = match overrides.get("app") {
        Some(value) => serde_json::from_value::<PathBuf>(value.clone())?,
        None => select_app(root, discovery_dir, profile.as_ref(), &exclude, selection)?,
    };
    crate::config::relative(&app)?;
    let profile = profile.unwrap_or_else(|| {
        if root.join(&app).join("src-tauri/tauri.conf.json").is_file() {
            Profile::Tauri
        } else {
            Profile::Standalone
        }
    });
    let tauri = profile == Profile::Tauri;
    let cargo_app = if tauri {
        in_app(&app, "src-tauri")
    } else {
        app.clone()
    };
    let cargo = manifest(&root.join(&cargo_app).join("Cargo.toml"))?;
    let package = package_value(&cargo, "name")
        .and_then(toml::Value::as_str)
        .context("Нет package.name в Cargo.toml приложения")?;
    let binary = if tauri {
        package.to_owned()
    } else if let Some(value) = overrides.pointer("/rust/binary") {
        serde_json::from_value::<String>(value.clone())?
    } else if let Some(name) = package_value(&cargo, "default-run").and_then(toml::Value::as_str) {
        name.to_owned()
    } else {
        let names = binaries(&root.join(&app), &cargo)?;
        selection.choose(Choice::Binary(package), names.into_iter().collect())?
    };
    let slug: String = match overrides.get("slug") {
        Some(value) => serde_json::from_value(value.clone())?,
        None => if tauri { package } else { &binary }.to_ascii_lowercase(),
    };
    let prefix = binary.to_ascii_uppercase().replace('-', "_");
    let release_config = PathBuf::from("src-tauri/tauri.release.conf.json");
    let mut config = json!({
        "slug": slug,
        "profile": profile,
        "app": app,
        "targets": [
            {"os": "linux", "arch": "x86_64"},
            {"os": "windows", "arch": "x86_64"},
            {"os": "macos", "arch": if tauri { "universal" } else { "aarch64" }},
        ],
        "exclude": [],
        "push_tag": false,
        "frontend": frontend(root, &app, &overrides, tauri)?,
        "rust": if tauri { Value::Null } else { json!({
            "binary": binary, "version_env": format!("{prefix}_RELEASE_VERSION"),
            "toolchain": "stable", "features": [], "version_args": ["--version"],
        }) },
        "tauri": if tauri { json!({
            "release_config": if root.join(&app).join(&release_config).is_file() { Some(&release_config) } else { None },
            "toolchain": "stable", "docker_rust_version": "1.91.1",
        }) } else { Value::Null },
        "signing": if tauri { json!({
            "private_key_env": "TAURI_SIGNING_PRIVATE_KEY",
            "password_env": "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
            "private_key_file": format!("~/.tauri/{slug}.key"),
        }) } else { json!({
            "identity": standalone_identity(root, &app),
            "private_key_env": format!("{prefix}_SIGNING_KEY"),
            "password_env": format!("{prefix}_KEY_PASSPHRASE"),
            "private_key_file": format!("~/.{slug}-signing/publisher.private.pem"),
        }) },
    });
    merge(&mut config, overrides);
    serde_json::from_value(config).context("Неверные настройки release.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Project, git, test_support::Fixture};
    use sha2::{Digest, Sha256};

    #[test]
    fn standalone_without_config_loads_from_subdirectory_and_snapshot() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        fs::create_dir(f.root.join("frontend")).unwrap();
        fs::write(
            f.root.join("frontend/package.json"),
            r#"{"scripts":{"build":"vite build"}}"#,
        )
        .unwrap();
        f.commit("feat: default configuration");
        let project = Project::discover(&f.root.join("src"), Selection::default()).unwrap();
        assert_eq!(project.config.profile, Profile::Standalone);
        assert_eq!(project.config.slug, "sample");
        assert_eq!(
            project.config.rust.as_ref().unwrap().version_env,
            "SAMPLE_RELEASE_VERSION"
        );
        assert_eq!(
            project.config.frontend.as_ref().unwrap().path,
            Path::new("frontend")
        );
        assert_eq!(project.config.signing.private_key_env, "SAMPLE_SIGNING_KEY");
        assert_eq!(project.base_version.to_string(), "0.2.4");
        assert!(!project.config.push_tag);
        assert!(!f.root.join("release.toml").exists());
        let source = f.temp.path().join("snapshot");
        git::snapshot(
            &f.root,
            &git::head(&f.root).unwrap(),
            &source,
            &f.temp.path().join("source.tar"),
        )
        .unwrap();
        let staged = project.reload_at(source).unwrap();
        assert_eq!(project.fingerprint, staged.fingerprint);
        assert!(!staged.root.join(".git").exists());
    }

    #[test]
    fn partial_settings_override_only_supplied_fields_and_reject_typos() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        let config = f.root.join("release.toml");
        fs::write(&config, "exclude = ['independent']\n[rust]\nfeatures = ['custom']\n[signing]\nprivate_key_file = '/local/key.pem'\n").unwrap();
        let project = f.discover().unwrap();
        let rust = project.config.rust.unwrap();
        assert_eq!(rust.binary, "sample");
        assert_eq!(rust.version_env, "SAMPLE_RELEASE_VERSION");
        assert_eq!(rust.features, ["custom"]);
        assert_eq!(
            project.config.signing.private_key_file.unwrap(),
            Path::new("/local/key.pem")
        );
        assert_eq!(project.config.exclude, [PathBuf::from("independent")]);
        assert_eq!(project.config.targets.len(), 3);
        fs::write(&config, "[rust]\nfeautres = ['typo']\n").unwrap();
        let error = format!("{:#}", f.discover().unwrap_err());
        assert!(error.contains("unknown field `feautres`"), "{error}");
        fs::write(&config, "[not valid toml").unwrap();
        assert!(f.discover().is_err());
    }

    #[test]
    fn linux_only_release_contains_the_executable_and_its_signature() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        let config = f.root.join("release.toml");
        fs::write(&config, "targets = [{ os = 'linux', arch = 'x86_64' }]").unwrap();
        f.commit("fix: Исправлена сборка");
        let project = f.discover().unwrap();
        let plan = crate::plan::calculate(&project).unwrap();
        let specs = crate::artifacts::specs(&project, &plan);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].os, "linux");
        assert_eq!(specs[0].signature.as_ref(), Some(&specs[1].name));

        fs::write(config, "targets = []").unwrap();
        assert!(f.discover().is_err());
    }

    #[test]
    fn missing_explicit_config_is_an_error_but_implicit_config_is_optional() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        assert!(Project::load(&f.root.join("release.toml"), Selection::default()).is_err());
        let absent = f.discover().unwrap();
        fs::write(f.root.join("release.toml"), "# Only defaults\n").unwrap();
        assert_eq!(absent.fingerprint, f.discover().unwrap().fingerprint);
    }

    #[test]
    fn missing_signing_identity_explains_how_to_create_or_locate_it() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        fs::remove_file(f.root.join("release.config.json")).unwrap();
        let error = f.discover().unwrap_err().to_string();
        for hint in [
            "release.config.json",
            "Создайте",
            "\"product_id\"",
            "\"public_key\"",
            "Ed25519",
            "signing.identity",
            "release.toml",
        ] {
            assert!(error.contains(hint), "{error}");
        }

        let settings = f.root.join("custom.toml");
        fs::write(&settings, "[signing]\nidentity = 'keys/product.json'\n").unwrap();
        let error = Project::load(&settings, Selection::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("keys/product.json"), "{error}");
        assert!(error.contains("custom.toml"), "{error}");
    }

    #[test]
    fn multiple_binaries_require_a_default_or_an_override() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        let unambiguous = f.discover().unwrap();
        fs::create_dir(f.root.join("src/bin")).unwrap();
        fs::write(f.root.join("src/bin/admin.rs"), "fn main() {}\n").unwrap();
        assert!(unambiguous.reload_at(f.root.clone()).is_err());
        let error = format!("{:#}", f.discover().unwrap_err());
        assert!(error.contains("admin, sample"), "{error}");
        assert!(error.contains("rust.binary"), "{error}");
        fs::write(f.root.join("release.toml"), "[rust]\nbinary = 'admin'\n").unwrap();
        assert_eq!(f.discover().unwrap().config.rust.unwrap().binary, "admin");
        fs::remove_file(f.root.join("release.toml")).unwrap();
        fs::write(
            f.root.join("Cargo.toml"),
            "[package]\nname='sample'\nversion='0.2.4'\ndefault-run='sample'\n",
        )
        .unwrap();
        assert_eq!(f.discover().unwrap().config.rust.unwrap().binary, "sample");
    }

    #[test]
    fn explicit_bin_paths_offer_only_their_cargo_names() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        fs::create_dir_all(f.root.join("src/bin/tool")).unwrap();
        fs::write(f.root.join("src/bin/tool/main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            f.root.join("Cargo.toml"),
            "[package]\nname='sample'\nversion='0.2.4'\nedition='2024'\n\
             [[bin]]\nname='server'\npath='src/main.rs'\n\
             [[bin]]\nname='admin'\npath='./src/bin/tool/main.rs'\n",
        )
        .unwrap();
        let project = Project::discover(
            &f.root,
            Selection::with_prompt(|_, options, _| {
                assert_eq!(options, ["admin", "server"]);
                Ok(1)
            }),
        )
        .unwrap();
        assert_eq!(project.config.slug, "server");
    }

    fn create_tauri_app(app: &Path) {
        let tauri = app.join("src-tauri");
        fs::create_dir_all(tauri.join("src")).unwrap();
        fs::write(tauri.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            tauri.join("Cargo.toml"),
            "[package]\nname='sample-desktop'\nversion='0.2.4'\n",
        )
        .unwrap();
        fs::write(tauri.join("tauri.conf.json"), r#"{"identifier":"com.example.sample","version":"0.2.4","plugins":{"updater":{"pubkey":"pinned-key"}}}"#).unwrap();
        fs::write(
            app.join("package.json"),
            r#"{"scripts":{"build":"vite build"}}"#,
        )
        .unwrap();
    }

    #[test]
    fn tauri_is_discovered_in_a_monorepo_without_a_release_overlay() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        fs::remove_file(f.root.join("src/main.rs")).unwrap();
        let app = f.root.join("apps/desktop");
        create_tauri_app(&app);
        let tauri = app.join("src-tauri");
        let project = f.discover().unwrap();
        assert_eq!(project.config.profile, Profile::Tauri);
        assert_eq!(project.config.app, Path::new("apps/desktop"));
        assert_eq!(project.config.slug, "sample-desktop");
        assert_eq!(project.config.targets[2].arch, "universal");
        assert!(
            project
                .config
                .tauri
                .as_ref()
                .unwrap()
                .release_config
                .is_none()
        );
        assert_eq!(project.identifier(), "com.example.sample");
        let other = f.root.join("apps/other/src-tauri");
        fs::create_dir_all(&other).unwrap();
        fs::copy(tauri.join("tauri.conf.json"), other.join("tauri.conf.json")).unwrap();
        let chosen = Project::discover(
            &f.root,
            Selection::with_prompt(|_, options, _| {
                assert_eq!(options, ["apps/desktop", "apps/other"]);
                Ok(0)
            }),
        )
        .unwrap();
        assert_eq!(chosen.config.app, Path::new("apps/desktop"));
        assert!(format!("{:#}", f.discover().unwrap_err()).contains("несколько Tauri-приложений"));
        fs::write(f.root.join("release.toml"), "exclude = ['apps/other']\n").unwrap();
        assert_eq!(f.discover().unwrap().config.app, Path::new("apps/desktop"));
        fs::write(
            tauri.join("tauri.release.conf.json"),
            r#"{"bundle":{"active":true}}"#,
        )
        .unwrap();
        assert!(
            f.discover()
                .unwrap()
                .config
                .tauri
                .unwrap()
                .release_config
                .is_some()
        );
        fs::write(
            tauri.join("tauri.release.conf.json"),
            r#"{"identifier":"replaced"}"#,
        )
        .unwrap();
        assert!(
            format!("{:#}", f.discover().unwrap_err()).contains("release_config не должен менять")
        );
    }

    #[test]
    fn mixed_apps_can_be_selected_from_the_root_or_their_own_directory() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        let server = f.root.join("crates/server");
        fs::create_dir_all(&server).unwrap();
        for name in ["Cargo.toml", "src", "release.config.json"] {
            fs::rename(f.root.join(name), server.join(name)).unwrap();
        }
        create_tauri_app(&f.root);
        fs::write(
            f.root.join("Cargo.toml"),
            "[workspace]\nmembers=['src-tauri','crates/server']\n",
        )
        .unwrap();
        f.commit("feat: add desktop and server");

        let error = f.discover().unwrap_err().to_string();
        assert!(
            error.contains("Rust/Tauri-приложений: ., crates/server"),
            "{error}"
        );
        let chosen = Project::discover(
            &f.root,
            Selection::with_prompt(|_, options, _| {
                assert_eq!(options, [".", "crates/server"]);
                Ok(1)
            }),
        )
        .unwrap();
        assert_eq!(chosen.config.app, Path::new("crates/server"));
        assert_eq!(chosen.config.profile, Profile::Standalone);

        let nested = Project::discover(&server.join("src"), Selection::default()).unwrap();
        assert_eq!(nested.fingerprint, chosen.fingerprint);
        let source = f.temp.path().join("snapshot");
        git::snapshot(
            &f.root,
            &git::head(&f.root).unwrap(),
            &source,
            &f.temp.path().join("source.tar"),
        )
        .unwrap();
        for project in [chosen, nested] {
            assert_eq!(
                project.reload_at(source.clone()).unwrap().fingerprint,
                project.fingerprint
            );
        }
        let desktop =
            Project::discover(&f.root.join("src-tauri/src"), Selection::default()).unwrap();
        assert_eq!(desktop.config.profile, Profile::Tauri);

        for settings in [
            "app = '.'",
            "profile = 'tauri'",
            "exclude = ['crates/server']",
        ] {
            fs::write(f.root.join("release.toml"), settings).unwrap();
            assert_eq!(
                Project::discover(&server, Selection::default())
                    .unwrap()
                    .config
                    .profile,
                Profile::Tauri
            );
        }
        fs::write(f.root.join("release.toml"), "profile = 'standalone'").unwrap();
        assert_eq!(f.discover().unwrap().config.app, Path::new("crates/server"));
    }

    #[test]
    fn standalone_can_use_its_flat_update_identity_for_signing() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        fs::write(
            f.root.join("release.toml"),
            format!(
                "[signing]\nprivate_key_file = '{}'\n",
                f.temp.path().join("test-key.pem").display()
            ),
        )
        .unwrap();
        let original = f.discover().unwrap();
        let path = "update-identity.json";
        let identity = json!({
            "product_id": original.identifier(),
            "public_key": original.identity["updates"]["public_key"],
        });
        fs::write(f.root.join(path), serde_json::to_vec(&identity).unwrap()).unwrap();
        assert_eq!(f.discover().unwrap().fingerprint, original.fingerprint);
        fs::remove_file(f.root.join("release.config.json")).unwrap();
        let project = f.discover().unwrap();
        assert_eq!(
            project.config.signing.identity.as_deref(),
            Some(Path::new(path))
        );
        assert_eq!(project.identifier(), original.identifier());
        crate::signing::Material::load(&project).unwrap();
    }

    #[test]
    fn ambiguous_apps_need_only_an_app_override() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        fs::remove_file(f.root.join("src/main.rs")).unwrap();
        for name in ["first", "second"] {
            let app = f.root.join("apps").join(name);
            fs::create_dir_all(app.join("src")).unwrap();
            fs::write(
                app.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='0.2.4'\n"),
            )
            .unwrap();
            fs::write(app.join("src/main.rs"), "fn main() {}\n").unwrap();
        }
        let error = format!("{:#}", f.discover().unwrap_err());
        assert!(error.contains("несколько Rust-приложений"), "{error}");
        assert!(error.contains("apps/first, apps/second"), "{error}");
        fs::write(
            f.root.join("release.toml"),
            "[signing]\nidentity = 'release.config.json'\n",
        )
        .unwrap();
        let chosen = Project::discover(
            &f.root,
            Selection::with_prompt(|_, options, _| {
                assert_eq!(options, ["apps/first", "apps/second"]);
                Ok(1)
            }),
        )
        .unwrap();
        assert_eq!(chosen.config.app, Path::new("apps/second"));
        assert_eq!(
            chosen.reload_at(f.root.clone()).unwrap().fingerprint,
            chosen.fingerprint
        );
        fs::write(
            f.root.join("release.toml"),
            "app = 'apps/second'\n[signing]\nidentity = 'release.config.json'\n",
        )
        .unwrap();
        assert_eq!(f.discover().unwrap().config.slug, "second");
    }

    #[test]
    fn complete_configs_keep_existing_release_fingerprints() {
        let f = Fixture::new("0.2.4", false);
        let project = f.project();
        let mut hash = Sha256::new();
        hash.update(fs::read(f.root.join("release.toml")).unwrap());
        hash.update(serde_json::to_vec(&project.identity).unwrap());
        assert_eq!(project.fingerprint, format!("{:x}", hash.finalize()));
    }

    #[test]
    fn detected_frontend_can_be_disabled_with_one_setting() {
        let f = Fixture::new("0.2.4", false);
        f.use_defaults();
        fs::create_dir(f.root.join("frontend")).unwrap();
        fs::write(
            f.root.join("frontend/package.json"),
            r#"{"scripts":{"build":"vite build"}}"#,
        )
        .unwrap();
        fs::write(f.root.join("release.toml"), "[frontend]\nenabled = false\n").unwrap();
        let frontend = f.discover().unwrap().config.frontend.unwrap();
        assert!(!frontend.enabled);
        assert_eq!(frontend.path, Path::new("frontend"));
    }
}
