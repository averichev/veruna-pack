use anyhow::{Context, Result, bail};
use dialoguer::{Select, theme::ColorfulTheme};
use std::{
    ffi::{OsStr, OsString},
    io::{self, IsTerminal},
};

pub enum Choice<'a> {
    Application(&'static str),
    Binary(&'a str),
}

type Prompt = fn(&str, &[String], usize) -> Result<usize>;

#[derive(Clone, Debug, Default)]
pub struct Selection {
    prompt: Option<Prompt>,
    application: Option<OsString>,
    binary: Option<OsString>,
}

impl Selection {
    pub fn for_cli(json: bool) -> Self {
        let ci = std::env::var("CI")
            .is_ok_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"));
        if !json
            && !ci
            && io::stdin().is_terminal()
            && io::stdout().is_terminal()
            && io::stderr().is_terminal()
        {
            Self::with_prompt(prompt)
        } else {
            Self::default()
        }
    }

    pub fn with_prompt(prompt: Prompt) -> Self {
        Self {
            prompt: Some(prompt),
            ..Self::default()
        }
    }

    pub fn replay(&self) -> Self {
        Self {
            prompt: None,
            ..self.clone()
        }
    }

    pub fn choose<T: AsRef<OsStr>>(
        &mut self,
        choice: Choice<'_>,
        mut options: Vec<T>,
    ) -> Result<T> {
        let answer = match choice {
            Choice::Application(_) => &mut self.application,
            Choice::Binary(_) => &mut self.binary,
        };
        if let Some(previous) = answer.as_deref() {
            return options
                .into_iter()
                .find(|item| item.as_ref() == previous)
                .with_context(|| {
                    format!(
                        "Выбранная цель {} больше недоступна",
                        previous.to_string_lossy().escape_debug()
                    )
                });
        }
        match options.len() {
            0 => match choice {
                Choice::Application(_) => bail!(
                    "Не найдено Rust/Tauri-приложение. Для нестандартной структуры укажите app в release.toml"
                ),
                Choice::Binary(package) => bail!(
                    "В пакете {package} не найдено исполняемых файлов Cargo. Проверьте цели [[bin]] в Cargo.toml"
                ),
            },
            1 => return Ok(options.pop().unwrap()),
            _ => {}
        }
        let labels: Vec<_> = options
            .iter()
            .map(|item| item.as_ref().to_string_lossy().escape_debug().to_string())
            .collect();
        let (title, hint, default) = match choice {
            Choice::Application(kind) => (
                format!("Найдено несколько {kind}-приложений"),
                "Укажите app в release.toml",
                0,
            ),
            Choice::Binary(package) => (
                format!(
                    "Выберите исполняемый файл пакета {}",
                    package.escape_debug()
                ),
                "Задайте package.default-run в Cargo.toml или rust.binary в release.toml",
                options
                    .iter()
                    .position(|item| item.as_ref() == OsStr::new(package))
                    .unwrap_or(0),
            ),
        };
        let Some(select) = self.prompt else {
            bail!("{title}: {}. {hint}", labels.join(", "));
        };
        let index = select(&title, &labels, default)?;
        let selected = options.swap_remove(index);
        *answer = Some(selected.as_ref().to_owned());
        Ok(selected)
    }
}

fn prompt(title: &str, labels: &[String], default: usize) -> Result<usize> {
    eprintln!("↑/↓ или Tab/Shift+Tab — перемещение, Enter — выбор, Esc — отмена");
    Select::with_theme(&ColorfulTheme::default())
        .with_prompt(title)
        .items(labels)
        .default(default)
        .interact_opt()
        .context("Не удалось прочитать выбор в терминале")?
        .context("Выбор отменён")
}
