use crate::config::Project;
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub fn command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    // The caller's Git redirections must not select a different repository/index.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        cmd.env_remove(key);
    }
    cmd
}

pub fn bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = command(root)
        .args(args)
        .output()
        .context("Не удалось запустить Git")?;
    ensure!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}
pub fn read(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(bytes(root, args)?)?
        .trim_end_matches('\n')
        .to_owned())
}
pub fn head(root: &Path) -> Result<String> {
    read(root, &["rev-parse", "HEAD^{commit}"])
}

pub fn complete(root: &Path) -> Result<()> {
    let hint = "Неполная история Git. Получите недостающие теги и коммиты: git fetch --unshallow --tags origin (для полного clone: git fetch --tags origin)";
    ensure!(
        read(root, &["rev-parse", "--is-shallow-repository"])? == "false",
        "{hint}"
    );
    let partial = command(root)
        .args(["config", "--get", "extensions.partialClone"])
        .output()?;
    ensure!(
        !partial.status.success(),
        "{hint}; partial clone не поддерживается"
    );
    let promisor = command(root)
        .args([
            "config",
            "--bool",
            "--get-regexp",
            r"^remote\..*\.promisor$",
        ])
        .output()?;
    ensure!(
        !String::from_utf8_lossy(&promisor.stdout)
            .lines()
            .any(|line| line.ends_with(" true")),
        "{hint}; promisor/partial clone не поддерживается"
    );
    let tags = command(root)
        .args(["config", "--get", "remote.origin.tagOpt"])
        .output()?;
    ensure!(
        String::from_utf8_lossy(&tags.stdout).trim() != "--no-tags",
        "{hint}; remote.origin.tagOpt=--no-tags"
    );
    let objects =
        read(root, &["rev-list", "--objects", "--missing=print", "HEAD"]).context(hint)?;
    ensure!(!objects.lines().any(|s| s.starts_with('?')), "{hint}");
    Ok(())
}

pub fn tag_commit(root: &Path, tag: &str) -> Result<Option<String>> {
    let reference = format!("refs/tags/{tag}^{{commit}}");
    let output = command(root)
        .args(["rev-parse", "--verify", "--quiet", &reference])
        .output()?;
    if output.status.success() {
        Ok(Some(String::from_utf8(output.stdout)?.trim().into()))
    } else {
        let exists = command(root)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/tags/{tag}"),
            ])
            .status()?;
        ensure!(
            !exists.success(),
            "Тег {tag} существует, но не указывает на коммит"
        );
        Ok(None)
    }
}

pub fn check_tag(root: &Path, tag: &str, commit: &str) -> Result<()> {
    if let Some(existing) = tag_commit(root, tag)? {
        ensure!(
            existing == commit,
            "Конфликт тега {tag}: {existing}, собранный коммит {commit}. Тег сохранён."
        );
    }
    Ok(())
}

pub fn clean(project: &Project) -> Result<()> {
    let tracked = bytes(&project.root, &["ls-files", "-z", "--", "releases"])?;
    ensure!(
        tracked.is_empty(),
        "releases/ должен содержать только выходные файлы и не отслеживаться Git"
    );
    let mut args = vec![
        "status".to_owned(),
        "--porcelain=v1".into(),
        "-z".into(),
        "--untracked-files=all".into(),
        "--".into(),
        ".".into(),
        ":(exclude,literal)releases".into(),
    ];
    for p in &project.config.exclude {
        args.push(format!(":(exclude,literal){}", p.display()));
    }
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let status = bytes(&project.root, &refs)?;
    ensure!(
        status.is_empty(),
        "Исходники изменены или есть неотслеживаемые файлы. Закоммитьте их перед сборкой.\n{}",
        String::from_utf8_lossy(&status).replace('\0', "\n")
    );
    let modes = read(&project.root, &["ls-files", "--stage"])?;
    ensure!(
        !modes.lines().any(|s| s.starts_with("160000 ")),
        "Git submodules пока не поддерживаются: требуется самодостаточный checkout"
    );
    Ok(())
}

pub fn common_dir(root: &Path) -> Result<PathBuf> {
    let value = PathBuf::from(read(root, &["rev-parse", "--git-common-dir"])?);
    Ok(if value.is_absolute() {
        value
    } else {
        root.join(value)
    })
}

pub fn snapshot(root: &Path, commit: &str, destination: &Path, archive: &Path) -> Result<()> {
    let output = command(root)
        .args(["archive", "--format=tar", commit])
        .stdout(fs::File::create(archive)?)
        .stderr(Stdio::piped())
        .output()?;
    ensure!(
        output.status.success(),
        "git archive: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::create_dir(destination)?;
    tar::Archive::new(fs::File::open(archive)?).unpack(destination)?;
    validate_snapshot(destination, destination)?;
    Ok(())
}

fn validate_snapshot(root: &Path, directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            ensure!(
                entry.path().canonicalize()?.starts_with(root),
                "Snapshot содержит внешний symlink: {}",
                entry.path().display()
            );
        } else if entry.file_type()?.is_dir() {
            validate_snapshot(root, &entry.path())?;
        }
    }
    Ok(())
}

pub fn annotate(root: &Path, tag: &str, commit: &str, changelog: &str) -> Result<()> {
    check_tag(root, tag, commit)?;
    let reference = format!("refs/tags/{tag}");
    let existing = command(root)
        .args(["rev-parse", "--verify", "--quiet", &reference])
        .output()?;
    let old = if existing.status.success() {
        String::from_utf8(existing.stdout)?.trim().to_owned()
    } else {
        "0".repeat(commit.len())
    };
    if !old.bytes().all(|b| b == b'0') && read(root, &["cat-file", "-t", &old])? == "tag" {
        let object = String::from_utf8(bytes(root, &["cat-file", "tag", &old])?)?;
        if object.split_once("\n\n").map(|(_, message)| message) == Some(changelog) {
            return Ok(());
        }
    }
    let ident = read(root, &["var", "GIT_COMMITTER_IDENT"])?;
    let object = format!("object {commit}\ntype commit\ntag {tag}\ntagger {ident}\n\n{changelog}");
    let mut child = command(root)
        .arg("mktag")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(object.as_bytes())?;
    let result = child.wait_with_output()?;
    ensure!(
        result.status.success(),
        "Не удалось создать аннотацию: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let new = String::from_utf8(result.stdout)?;
    read(root, &["update-ref", &reference, new.trim(), &old])
        .context("Тег изменился параллельно; готовый выпуск сохранён, повторите build")?;
    Ok(())
}
