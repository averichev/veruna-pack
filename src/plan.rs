use crate::{
    artifacts,
    config::{Project, stable_version},
    git,
};
use anyhow::{Result, ensure};
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::LazyLock,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    pub source_commit: String,
    pub previous_tag: Option<String>,
    pub tag: Option<String>,
    pub version: Option<Version>,
    pub bump: Option<String>,
    pub platforms: Vec<String>,
    pub files: Vec<String>,
    pub changelog: String,
}

static HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([a-z][a-z0-9-]*)(?:\(([^()\r\n]+)\))?(!)?:\s+(.+)$").unwrap());
static FOOTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z -]*(?:: | #)").unwrap());

#[derive(Debug)]
struct Change {
    impact: u8,
    group: usize,
    text: String,
}

fn change(message: &str) -> Option<Change> {
    let mut lines = message.lines();
    let header = lines.next()?;
    let captures = HEADER.captures(header)?;
    let kind = captures.get(1)?.as_str();
    let scope = captures.get(2).map(|s| s.as_str());
    let mut explanation = Vec::new();
    let mut in_breaking = false;
    let mut breaking = captures.get(3).is_some();
    for line in lines {
        if let Some(text) = line
            .strip_prefix("BREAKING CHANGE:")
            .or_else(|| line.strip_prefix("BREAKING-CHANGE:"))
        {
            breaking = true;
            in_breaking = true;
            explanation.push(text.trim().to_owned());
        } else if in_breaking {
            if FOOTER.is_match(line) {
                in_breaking = false;
            } else {
                explanation.push(line.to_owned());
            }
        }
    }
    let (impact, group) = if breaking {
        (3, 0)
    } else {
        match kind {
            "feat" => (2, 1),
            "fix" => (1, 2),
            "perf" => (1, 3),
            "deps" => (1, 4),
            "revert" => (1, 5),
            _ => return None,
        }
    };
    let title = captures.get(4)?.as_str();
    let mut text = if let Some(scope) = scope {
        format!("**{scope}:** {title}")
    } else {
        title.to_owned()
    };
    let explanation = explanation.join("\n");
    if !explanation.trim().is_empty() {
        text.push_str("\n\n");
        text.push_str(explanation.trim());
    }
    Some(Change {
        impact,
        group,
        text,
    })
}

const GROUPS: [&str; 6] = [
    "Что изменится при обновлении",
    "Новое",
    "Исправления",
    "Производительность",
    "Обновления зависимостей",
    "Откаты",
];

pub fn calculate(project: &Project) -> Result<Plan> {
    git::complete(&project.root)?;
    let sha = git::head(&project.root)?;
    let tags = git::read(&project.root, &["tag", "--color=never", "--merged", &sha])?;
    let mut stable = Vec::new();
    for tag in tags.lines() {
        if let Some(value) = tag.strip_prefix('v')
            && let Ok(version) = stable_version(value)
        {
            stable.push((
                version,
                tag.to_owned(),
                git::tag_commit(&project.root, tag)?.unwrap(),
            ));
        }
    }
    stable.sort_by(|a, b| b.0.cmp(&a.0));
    let at_head = stable.iter().find(|(_, _, commit)| commit == &sha);
    let previous = stable
        .iter()
        .find(|(_, tag, _)| at_head.is_none_or(|(_, current, _)| tag != current));
    if let (Some(current), Some(previous)) = (at_head, previous) {
        ensure!(
            current.0 > previous.0,
            "Версия тега на HEAD не выше предыдущего достижимого релиза"
        );
    }
    let base = previous.map(|(v, _, _)| v).unwrap_or(&project.base_version);
    let range = previous
        .map(|(_, tag, _)| format!("{tag}..{sha}"))
        .unwrap_or_else(|| sha.clone());
    let log = git::bytes(
        &project.root,
        &["log", "--reverse", "--format=%H%x00%B%x00", &range],
    )?;
    let log = String::from_utf8(log)?;
    let mut parts = log.split('\0');
    let mut groups: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut impact = 0;
    let mut seen = BTreeSet::new();
    while let (Some(commit), Some(message)) = (parts.next(), parts.next()) {
        let commit = commit.trim();
        if commit.is_empty() {
            continue;
        }
        ensure!(
            [40, 64].contains(&commit.len()) && commit.bytes().all(|c| c.is_ascii_hexdigit()),
            "Неверный SHA в Git log (сообщения с NUL не поддерживаются)"
        );
        if !seen.insert(commit) {
            continue;
        }
        let Some(change) = change(message) else {
            continue;
        };
        let paths = git::bytes(
            &project.root,
            &[
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-m",
                "-z",
                commit,
            ],
        )?;
        let paths: Vec<_> = paths.split(|b| *b == 0).filter(|p| !p.is_empty()).collect();
        if !paths.is_empty()
            && paths
                .iter()
                .all(|p| std::str::from_utf8(p).is_ok_and(|s| project.excluded(Path::new(s))))
        {
            continue;
        }
        impact = impact.max(change.impact);
        groups.entry(change.group).or_default().push(change.text);
    }
    let mut bump = None;
    let version = if let Some((version, _, _)) = at_head {
        Some(version.clone())
    } else if impact == 0 {
        None
    } else {
        let mut version = base.clone();
        let operation = if impact == 3 && version.major >= 1 {
            version.major = version
                .major
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("SemVer overflow"))?;
            version.minor = 0;
            version.patch = 0;
            "major"
        } else if impact >= 2 {
            version.minor = version
                .minor
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("SemVer overflow"))?;
            version.patch = 0;
            "minor"
        } else {
            version.patch = version
                .patch
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("SemVer overflow"))?;
            "patch"
        };
        bump = Some(operation.into());
        Some(version)
    };
    let tag = version.as_ref().map(|v| format!("v{v}"));
    if let Some(tag) = &tag {
        git::check_tag(&project.root, tag, &sha)?;
    }
    let mut changelog = String::new();
    if let Some(version) = &version {
        changelog = format!("# Версия {version}\n");
        for (index, entries) in groups {
            changelog.push_str(&format!("\n## {}\n\n", GROUPS[index]));
            for entry in entries {
                changelog.push_str("- ");
                changelog.push_str(&entry.replace('\n', "\n  "));
                changelog.push('\n');
            }
        }
    }
    let mut plan = Plan {
        source_commit: sha,
        previous_tag: previous.map(|(_, t, _)| t.clone()),
        tag,
        version,
        bump,
        platforms: project
            .config
            .targets
            .iter()
            .map(|t| format!("{}-{}", t.os, t.arch))
            .collect(),
        files: Vec::new(),
        changelog,
    };
    if plan.version.is_some() {
        plan.files = artifacts::specs(project, &plan)
            .into_iter()
            .map(|s| s.name)
            .collect();
        plan.files.extend([
            "CHANGELOG.md".into(),
            "release.json".into(),
            "SHA256SUMS".into(),
        ]);
        plan.files.sort();
    }
    Ok(plan)
}

impl Plan {
    pub fn display(&self) -> String {
        let mut output = format!(
            "Исходный коммит: {}\nПредыдущий тег: {}\n",
            self.source_commit,
            self.previous_tag.as_deref().unwrap_or("нет")
        );
        if let Some(version) = &self.version {
            output.push_str(&format!(
                "Версия: {version}\nБудущий тег: {}\nПлатформы: {}\nФайлы:\n",
                self.tag.as_ref().unwrap(),
                self.platforms.join(", ")
            ));
            for file in &self.files {
                output.push_str(&format!("  releases/{version}/{file}\n"));
            }
            output.push('\n');
            output.push_str(&self.changelog);
        } else {
            output.push_str("Нового выпуска нет\n");
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conventional_and_breaking() {
        assert!(change("Merge branch 'topic'").is_none());
        assert!(change("chore(ci): cache").is_none());
        for kind in ["fix", "perf", "deps", "revert"] {
            assert_eq!(
                change(&format!("{kind}(client): details")).unwrap().impact,
                1
            );
        }
        assert_eq!(
            change("feat(search): поиск по ИНН").unwrap().text,
            "**search:** поиск по ИНН"
        );
        for marker in ["BREAKING CHANGE:", "BREAKING-CHANGE:"] {
            let c = change(&format!("refactor(api): новый формат\n\n{marker} перенесите поле\nи перезапустите\n\nReviewed-by: somebody")).unwrap();
            assert_eq!(c.impact, 3);
            assert!(c.text.contains("перенесите поле\nи перезапустите"));
            assert!(!c.text.contains("Reviewed-by"));
        }
        assert_eq!(change("chore!: incompatible").unwrap().group, 0);
    }

    #[test]
    fn acceptance_plan_and_tagged_head_have_identical_changes() {
        let f = crate::test_support::Fixture::seeded(false);
        let p = f.project();
        let before = f.git(&["status", "--porcelain", "--untracked-files=all"]);
        std::fs::remove_file(f.temp.path().join("test-key.pem")).unwrap();
        let plan = calculate(&p).unwrap();
        assert_eq!(plan.version.unwrap().to_string(), "0.3.0");
        assert_eq!(plan.previous_tag.as_deref(), Some("v0.2.4"));
        assert_eq!(plan.changelog.matches("\n- ").count(), 2);
        assert!(!plan.changelog.contains("кеш"));
        assert!(!f.root.join("releases").exists());
        assert_eq!(
            before,
            f.git(&["status", "--porcelain", "--untracked-files=all"])
        );
        f.base("v0.3.0");
        let tagged = calculate(&p).unwrap();
        assert_eq!(tagged.changelog, plan.changelog);
        assert_eq!(tagged.previous_tag.as_deref(), Some("v0.2.4"));
        assert_eq!(tagged.version.unwrap().to_string(), "0.3.0");
    }

    #[test]
    fn initial_and_breaking_rules() {
        for (seed, message, expected) in [
            ("0.2.4", "feat: new", Some("0.3.0")),
            ("0.2.4", "fix: repair", Some("0.2.5")),
            ("0.2.4", "feat!: incompatible", Some("0.3.0")),
            (
                "1.2.4",
                "refactor: internals\n\nBREAKING CHANGE: migrate",
                Some("2.0.0"),
            ),
            ("0.2.4", "docs: manual", None),
        ] {
            let f = crate::test_support::Fixture::new(seed, false);
            f.commit(message);
            assert_eq!(
                calculate(&f.project())
                    .unwrap()
                    .version
                    .map(|v| v.to_string())
                    .as_deref(),
                expected
            );
        }
    }

    #[test]
    fn technical_and_independent_product_commits_do_not_release() {
        let f = crate::test_support::Fixture::new("0.2.4", false);
        f.base("v0.2.4");
        f.commit("docs: guide");
        std::fs::create_dir(f.root.join("vpn-manager")).unwrap();
        std::fs::write(f.root.join("vpn-manager/product.txt"), "another product").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-m", "feat!: independent VPN change"]);
        assert!(calculate(&f.project()).unwrap().version.is_none());
        std::fs::write(
            f.root.join("vpn-manager/product.txt"),
            "dirty independent product",
        )
        .unwrap();
        git::clean(&f.project()).unwrap();
        f.commit("deps(sqlite): update");
        assert_eq!(
            calculate(&f.project())
                .unwrap()
                .version
                .unwrap()
                .to_string(),
            "0.2.5"
        );
    }

    #[test]
    fn semver_order_and_unreachable_tags() {
        let f = crate::test_support::Fixture::new("0.1.0", false);
        f.base("v0.9.9");
        f.commit("chore: base");
        f.base("v0.10.0");
        f.commit("fix: repair");
        let plan = calculate(&f.project()).unwrap();
        assert_eq!(plan.version.unwrap().to_string(), "0.10.1");
        f.git(&["checkout", "--orphan", "unrelated"]);
        f.commit("feat: unrelated");
        f.base("v9.0.0");
        f.git(&["checkout", "main"]);
        assert_eq!(
            calculate(&f.project()).unwrap().previous_tag.as_deref(),
            Some("v0.10.0")
        );
    }

    #[test]
    fn shallow_history_is_actionable_error() {
        let f = crate::test_support::Fixture::seeded(false);
        let clone = f.temp.path().join("shallow");
        let result = std::process::Command::new("git")
            .args(["clone", "--depth", "1"])
            .arg(format!("file://{}", f.root.display()))
            .arg(&clone)
            .output()
            .unwrap();
        assert!(result.status.success());
        let project = Project::load(&clone.join("release.toml"), Default::default()).unwrap();
        assert!(
            format!("{:#}", calculate(&project).unwrap_err())
                .contains("git fetch --unshallow --tags origin")
        );
    }

    #[test]
    fn promisor_history_fails_without_attempting_lazy_fetch() {
        let f = crate::test_support::Fixture::seeded(false);
        f.git(&["config", "remote.origin.promisor", "true"]);
        f.git(&[
            "config",
            "remote.origin.url",
            "https://invalid.example.invalid/repo",
        ]);
        assert!(format!("{:#}", calculate(&f.project()).unwrap_err()).contains("promisor/partial"));
    }

    #[test]
    fn multiple_tags_on_head_use_highest_and_an_empty_range_from_previous() {
        let f = crate::test_support::Fixture::new("0.1.0", false);
        f.base("v0.1.0");
        f.commit("feat: same source");
        f.base("v0.2.4");
        f.base("v0.3.0");
        let plan = calculate(&f.project()).unwrap();
        assert_eq!(plan.version.unwrap().to_string(), "0.3.0");
        assert_eq!(plan.previous_tag.as_deref(), Some("v0.2.4"));
        assert_eq!(plan.changelog, "# Версия 0.3.0\n");
    }
}
