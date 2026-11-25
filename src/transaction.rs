use crate::{
    artifacts, build as profiles,
    config::Project,
    git,
    plan::{self, Plan},
    signing::Material,
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

pub fn build(project: &Project) -> Result<()> {
    let lock_path = git::common_dir(&project.root)?.join("veruna-pack.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock.try_lock_exclusive()
        .context("В этом Git-репозитории уже запущен veruna-pack build")?;
    execute(project, profiles::run, artifacts::verify)
}

fn unchanged(project: &Project, expected: &Plan) -> Result<()> {
    git::clean(project)?;
    let reloaded = Project::from_root(project.root.clone(), project.config_path.clone())?;
    ensure!(
        reloaded.fingerprint == project.fingerprint,
        "Конфигурация изменилась во время сборки"
    );
    let actual = plan::calculate(&reloaded)?;
    ensure!(
        actual.source_commit == expected.source_commit
            && actual.previous_tag == expected.previous_tag
            && actual.tag == expected.tag
            && actual.version == expected.version
            && actual.changelog == expected.changelog
            && actual.files == expected.files,
        "Исходники или релизные теги изменились во время сборки; исходный коммит {}",
        expected.source_commit
    );
    Ok(())
}

fn execute(
    project: &Project,
    builder: impl Fn(&Project, &Plan, &Path, &Path, &Material) -> Result<()>,
    verify: impl Fn(&Project, &Plan, &Path) -> Result<()>,
) -> Result<()> {
    let plan = plan::calculate(project)?;
    let Some(version) = &plan.version else {
        println!("Нового выпуска нет");
        return Ok(());
    };
    git::clean(project)?;
    let output = project.root.join("releases");
    if output.try_exists()? {
        ensure!(
            fs::symlink_metadata(&output)?.file_type().is_dir(),
            "releases должен быть каталогом без symlink"
        );
    } else {
        fs::create_dir(&output)?;
    }
    let destination = output.join(version.to_string());
    if destination.try_exists()? {
        verify(project, &plan, &destination)
            .map_err(|error| release_conflict(project, &plan, &destination, error))?;
        unchanged(project, &plan)?;
    } else {
        // Private keys are loaded only for a new build; verification/recovery needs public keys only.
        let material = Material::load(project)?;
        let work = tempfile::Builder::new()
            .prefix(".work-")
            .tempdir_in(&output)?;
        let source = work.path().join("source");
        git::snapshot(
            &project.root,
            &plan.source_commit,
            &source,
            &work.path().join("source.tar"),
        )?;
        fs::remove_file(work.path().join("source.tar"))?;
        let staged_project = Project::from_root(source, project.config_path.clone()).context(
            "Snapshot должен содержать манифесты, закреплённую идентичность и используемые настройки",
        )?;
        ensure!(
            staged_project.fingerprint == project.fingerprint,
            "Локальная конфигурация отличается от Git snapshot"
        );
        let staged = work.path().join("release");
        fs::create_dir(&staged)?;
        let cache = git::common_dir(&project.root)?.join("veruna-pack-cache");
        let result = (|| -> Result<()> {
            builder(&staged_project, &plan, &staged, &cache, &material)?;
            artifacts::finish(&staged_project, &plan, &staged, &material)?;
            verify(&staged_project, &plan, &staged)?;
            unchanged(project, &plan)?;
            for entry in fs::read_dir(&staged)? {
                File::open(entry?.path())?.sync_all()?;
            }
            File::open(&staged)?.sync_all()?;
            ensure!(
                !destination.try_exists()?,
                "Каталог назначения появился во время сборки; сохранён без изменений"
            );
            publish(&staged, &destination)
                .context("Не удалось атомарно сохранить выпуск без перезаписи")?;
            File::open(&output)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            let diagnostics = work.keep();
            return Err(error).with_context(|| {
                format!(
                    "Сборка не завершена. Диагностика сохранена: {}",
                    diagnostics.display()
                )
            });
        }
    }
    let tag = plan.tag.as_ref().unwrap();
    git::annotate(&project.root, tag, &plan.source_commit, &plan.changelog)
        .context("Готовый выпуск сохранён. Повторите build, чтобы завершить локальный тег")?;
    if project.config.push_tag {
        git::push(&project.root, tag)?;
    }
    println!(
        "Готовый выпуск: {}\nТег: {tag}\nКоммит: {}",
        destination.display(),
        plan.source_commit
    );
    Ok(())
}

fn shell_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn conflict_backup(destination: &Path) -> Option<std::path::PathBuf> {
    let name = destination.file_name()?.to_string_lossy();
    for suffix in 1.. {
        let candidate = destination.with_file_name(format!(".conflict-{name}-{suffix}"));
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(candidate),
            Err(_) => return None,
            Ok(_) => (),
        }
    }
    None
}

fn release_conflict(
    project: &Project,
    plan: &Plan,
    destination: &Path,
    error: anyhow::Error,
) -> anyhow::Error {
    let cli = match &project.config_path {
        Some(path) => format!("veruna-pack --config {}", shell_path(path)),
        None => "veruna-pack".into(),
    };
    let mut message = format!(
        "Существующий выпуск конфликтует с планом или повреждён: {}\n\
         Ожидается версия {}; тег {}; исходный коммит {}\n\n\
         Причина:\n{error:#}\n\n\
         Файлы выпуска и Git-теги сохранены без изменений.\n\n\
         Как исправить:\n\
         1. Если выпуск уже опубликован: восстановите полный комплект из доверенной копии \
         и используйте его исходный коммит и настройки. Новые изменения выпускайте под следующей версией.\n\
         2. Если это незавершённая локальная сборка: сохраните каталог под другим именем и повторите build.\n",
        destination.display(),
        plan.version.as_ref().unwrap(),
        plan.tag.as_ref().unwrap(),
        plan.source_commit
    );
    if let Some(backup) = conflict_backup(destination) {
        message.push_str(&format!(
            "\nКоманды для варианта 2 (из корня проекта):\n  cd {}\n  mv -n {} {}\n  {cli} build\n",
            shell_path(&project.root),
            shell_path(destination),
            shell_path(&backup)
        ));
    }
    message.push_str(&format!("\nДля сверки текущего плана: {cli} plan"));
    anyhow::anyhow!(message)
}

fn publish(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let source = CString::new(source.as_os_str().as_bytes())?;
        let destination = CString::new(destination.as_os_str().as_bytes())?;
        // C strings remain alive through the syscall. Exclusive rename preserves
        // even an empty destination created after the userspace existence check.
        #[cfg(target_os = "macos")]
        let result =
            unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        ensure!(result == 0, "rename: {}", std::io::Error::last_os_error());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        ensure!(!destination.exists(), "Каталог назначения уже существует");
        fs::rename(source, destination)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, fake_build, verify_transaction};
    use std::cell::Cell;

    #[test]
    fn partial_config_builds_from_snapshot_and_repeats() {
        let f = Fixture::seeded(false);
        f.use_defaults();
        fs::write(
            f.root.join("release.toml"),
            format!(
                "[signing]\nprivate_key_file = '{}'\n",
                f.temp.path().join("test-key.pem").display()
            ),
        )
        .unwrap();
        f.commit("chore: use automatic release configuration");
        let project = Project::discover(&f.root).unwrap();
        execute(&project, fake_build, verify_transaction).unwrap();
        let tag = f.git(&["rev-parse", "v0.3.0"]);
        fs::remove_file(f.temp.path().join("test-key.pem")).unwrap();
        execute(
            &project,
            |_, _, _, _, _| panic!("repeat must not rebuild"),
            verify_transaction,
        )
        .unwrap();
        assert_eq!(f.git(&["rev-parse", "v0.3.0"]), tag);
    }

    #[test]
    fn success_and_repeat_preserve_tag_checksums_and_changelog() {
        let f = Fixture::seeded(false);
        let p = f.project();
        let original_plan = plan::calculate(&p).unwrap();
        execute(&p, fake_build, verify_transaction).unwrap();
        let sums = fs::read(f.release().join("SHA256SUMS")).unwrap();
        let tag = f.git(&["rev-parse", "v0.3.0"]);
        let object = git::bytes(&f.root, &["cat-file", "tag", "v0.3.0"]).unwrap();
        let annotation = String::from_utf8(object)
            .unwrap()
            .split_once("\n\n")
            .unwrap()
            .1
            .to_owned();
        assert_eq!(annotation, original_plan.changelog);
        assert_eq!(
            annotation,
            fs::read_to_string(f.release().join("CHANGELOG.md")).unwrap()
        );
        assert_eq!(
            git::tag_commit(&f.root, "v0.3.0").unwrap().unwrap(),
            original_plan.source_commit
        );
        fs::remove_file(f.temp.path().join("test-key.pem")).unwrap();
        execute(
            &p,
            |_, _, _, _, _| panic!("repeat must not rebuild"),
            verify_transaction,
        )
        .unwrap();
        assert_eq!(sums, fs::read(f.release().join("SHA256SUMS")).unwrap());
        assert_eq!(tag, f.git(&["rev-parse", "v0.3.0"]));
    }

    #[test]
    fn third_build_failure_publishes_neither_directory_nor_tag() {
        let f = Fixture::seeded(false);
        let p = f.project();
        let result = execute(
            &p,
            |project, plan, output, cache, material| {
                fake_build(project, plan, output, cache, material)?;
                anyhow::bail!("third platform failed")
            },
            verify_transaction,
        );
        assert!(result.is_err());
        assert!(!f.release().exists());
        assert!(git::tag_commit(&f.root, "v0.3.0").unwrap().is_none());
        assert!(
            fs::read_dir(f.root.join("releases")).unwrap().any(|e| e
                .unwrap()
                .path()
                .join("release")
                .is_dir())
        );
    }

    #[test]
    fn wrong_signature_publishes_neither_directory_nor_tag() {
        let f = Fixture::seeded(false);
        let p = f.project();
        let result = execute(&p, fake_build, |project, plan, output| {
            let wrong =
                Material::Standalone(Box::new(ed25519_dalek::SigningKey::from_bytes(&[9; 32])));
            artifacts::finish(project, plan, output, &wrong)?;
            verify_transaction(project, plan, output)
        });
        assert!(format!("{:#}", result.unwrap_err()).contains("подпись"));
        assert!(!f.release().exists());
        assert!(git::tag_commit(&f.root, "v0.3.0").unwrap().is_none());
    }

    #[test]
    fn conflicting_tag_is_reported_and_preserved() {
        let f = Fixture::seeded(false);
        let p = f.project();
        let sha = f.git(&["rev-parse", "v0.2.4"]);
        f.git(&["checkout", "--orphan", "other"]);
        f.commit("chore: another branch");
        f.base("v0.3.0");
        let conflict = f.git(&["rev-parse", "v0.3.0"]);
        f.git(&["checkout", "main"]);
        let result = execute(
            &p,
            |_, _, _, _, _| panic!("conflict must stop before build"),
            verify_transaction,
        );
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("Конфликт тега"));
        assert!(message.contains(&conflict));
        assert!(message.contains(&git::head(&f.root).unwrap()));
        assert!(!f.release().exists());
        assert_eq!(f.git(&["rev-parse", "v0.3.0"]), conflict);
        assert_eq!(f.git(&["rev-parse", "v0.2.4"]), sha);
    }

    #[test]
    fn resume_after_publication_finishes_tag_without_keys_or_build() {
        let f = Fixture::seeded(false);
        let p = f.project();
        execute(&p, fake_build, verify_transaction).unwrap();
        let sums = fs::read(f.release().join("SHA256SUMS")).unwrap();
        f.git(&["tag", "-d", "v0.3.0"]);
        fs::remove_file(f.temp.path().join("test-key.pem")).unwrap();
        execute(
            &p,
            |_, _, _, _, _| panic!("must only complete tag"),
            verify_transaction,
        )
        .unwrap();
        assert_eq!(sums, fs::read(f.release().join("SHA256SUMS")).unwrap());
        assert_eq!(f.git(&["cat-file", "-t", "v0.3.0"]), "tag");
    }

    #[cfg(unix)]
    #[test]
    fn resume_after_failed_push_sends_only_release_tag() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::seeded(true);
        let base = f.git(&["rev-parse", "v0.2.4"]);
        f.git(&[
            "tag",
            "--no-sign",
            "-a",
            "-f",
            "v0.2.4",
            &base,
            "-m",
            "base release",
        ]);
        f.git(&["config", "push.followTags", "true"]);
        let origin = f.bare_origin();
        let hook = origin.join("hooks/pre-receive");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
        let p = f.project();
        assert!(execute(&p, fake_build, verify_transaction).is_err());
        let sums = fs::read(f.release().join("SHA256SUMS")).unwrap();
        let tag = f.git(&["rev-parse", "v0.3.0"]);
        fs::remove_file(hook).unwrap();
        fs::remove_file(f.temp.path().join("test-key.pem")).unwrap();
        execute(
            &p,
            |_, _, _, _, _| panic!("must only push tag"),
            verify_transaction,
        )
        .unwrap();
        assert_eq!(sums, fs::read(f.release().join("SHA256SUMS")).unwrap());
        assert_eq!(tag, f.git(&["rev-parse", "v0.3.0"]));
        let refs = git::read(&origin, &["for-each-ref", "--format=%(refname)"]).unwrap();
        assert_eq!(refs, "refs/tags/v0.3.0");
    }

    #[test]
    fn corrupt_release_is_preserved_without_rebuild() {
        let f = Fixture::seeded(false);
        let p = f.project();
        execute(&p, fake_build, verify_transaction).unwrap();
        let file = f.release().join("sample-0.3.0-linux-x86_64");
        fs::write(&file, "damage").unwrap();
        let tag = f.git(&["rev-parse", "v0.3.0"]);
        assert!(
            execute(
                &p,
                |_, _, _, _, _| panic!("must preserve corrupt release"),
                verify_transaction
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(file).unwrap(), "damage");
        assert_eq!(tag, f.git(&["rev-parse", "v0.3.0"]));
    }

    #[test]
    fn source_change_during_build_blocks_publication() {
        let f = Fixture::seeded(false);
        let p = f.project();
        let result = execute(
            &p,
            |project, plan, output, cache, material| {
                assert_ne!(project.root, f.root);
                assert!(!project.root.join(".git").exists());
                fake_build(project, plan, output, cache, material)?;
                f.commit("fix: source changed while building");
                Ok(())
            },
            verify_transaction,
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("изменились во время сборки"));
        assert!(!f.release().exists());
        assert!(git::tag_commit(&f.root, "v0.3.0").unwrap().is_none());
    }

    #[test]
    fn dirty_sources_stop_before_keys_and_compilers() {
        let f = Fixture::seeded(false);
        let p = f.project();
        fs::write(f.root.join("untracked"), "do not omit me").unwrap();
        fs::remove_file(f.temp.path().join("test-key.pem")).unwrap();
        let called = Cell::new(false);
        assert!(
            execute(
                &p,
                |_, _, _, _, _| {
                    called.set(true);
                    Ok(())
                },
                verify_transaction
            )
            .is_err()
        );
        assert!(!called.get());
        assert!(!f.release().exists());
    }

    #[test]
    fn os_lock_prevents_concurrent_builds() {
        let f = Fixture::seeded(false);
        let p = f.project();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(git::common_dir(&f.root).unwrap().join("veruna-pack.lock"))
            .unwrap();
        lock.try_lock_exclusive().unwrap();
        assert!(format!("{:#}", build(&p).unwrap_err()).contains("уже запущен"));
    }

    #[test]
    fn exclusive_publication_preserves_even_empty_existing_directory() {
        let work = tempfile::tempdir().unwrap();
        let source = work.path().join("source");
        let destination = work.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), "content").unwrap();
        fs::create_dir(&destination).unwrap();
        assert!(publish(&source, &destination).is_err());
        assert!(source.join("file").is_file());
        assert!(!destination.join("file").exists());
    }
}
