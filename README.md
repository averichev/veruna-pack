# veruna-pack

Одна Rust CLI для локального выпуска приложений с Mac: версия из Git,
frontend, три платформы, подписи обновлений, changelog и аннотированный тег.
Профили вызывают инструменты сборки непосредственно; старые release-скрипты
продукта и его Dockerfile выпуска не нужны.

## Установка и обновление

На сборочном Mac нужны Git с полной историей и тегами, make, rustup/Rust 1.91+
с rustfmt/Clippy и Xcode Command Line Tools. Для frontend/Tauri также нужен
Node.js с npm, совместимый с зависимостями продукта; версия выбирается автоматически.
Установка самой CLI также требует системный C toolchain (зависимости сжатия).

```sh
git clone git@github.com:averichev/veruna-pack.git
cd veruna-pack
cargo install --locked --path . --root "$HOME/.local"
```

Добавьте каталог установки в PATH своей оболочки:

- **zsh/bash:** выполните `export PATH="$HOME/.local/bin:$PATH"` и сохраните
  эту строку в `~/.zshrc` / `~/.bashrc` соответственно.
- **fish:** выполните один раз `fish_add_path "$HOME/.local/bin"`;
  fish сохранит настройку для следующих сеансов.

Проверьте установку командой `veruna-pack --version`.
CLI устанавливается из каталога **утилиты**, а `veruna-pack build` запускается
из каталога **продукта**. `release.toml` нужен только для нестандартных настроек.

Для обновления в копии **утилиты**:

```sh
git pull --ff-only
cargo install --locked --path . --root "$HOME/.local" --force
```

Для standalone дополнительно нужны Zig, cargo-zigbuild 0.23.0 и MinGW-w64:

```sh
xcode-select --install
brew install zig mingw-w64
cargo install --locked --version 0.23.0 cargo-zigbuild
rustup target add --toolchain stable aarch64-apple-darwin \
  x86_64-unknown-linux-musl x86_64-pc-windows-gnu
```

Linux собирается статически через Zig/musl, Windows — через MinGW,
macOS — через Apple SDK. Для Tauri нужны `brew install sevenzip` (7zz для проверки
архитектуры приложения внутри NSIS), Docker Desktop с Buildx и работающим
Linux VM, а также `aarch64-apple-darwin` и `x86_64-apple-darwin` в выбранном
toolchain. Docker собирает Linux amd64 на Ubuntu 22.04; Windows x64 собирается
в нативном Linux-контейнере через cargo-xwin 0.23.1 и NSIS. cargo-xwin собирается
с поддерживаемым native-tls backend для получения SDK Microsoft. UTF-8 локаль
сохраняет кириллицу в именах Windows-приложения при работе NSIS. Образ CLI содержит
описание этих сборок (Rust 1.91.1, выбранная версия Node.js); в продукт его копировать не нужно.
Версию Rust контейнера можно задать через `tauri.docker_rust_version`.
Первое заполнение SDK, образов, npm и Cargo cache требует сети и места на диске.
Microsoft SDK используется на условиях его лицензии, как в прежнем процессе.
Продукт должен фиксировать Tauri CLI в package-lock.json.

## Подключение проекта

Из каталога продукта выполните:

```sh
veruna-pack plan
veruna-pack build
```

`release.toml` не требуется: CLI определяет приложение и параметры по его
существующим манифестам. `plan` читает проект и Git, не открывает закрытых
ключей, не собирает приложение и не создаёт файлов. `plan --json` возвращает
те же данные с полем `changelog`. Можно добавить две цели в Makefile:

```makefile
.PHONY: release release-plan

release:
	veruna-pack build

release-plan:
	veruna-pack plan
```

Параметры по умолчанию:

| Параметр | Откуда берётся |
| --- | --- |
| Корень | Git-репозиторий, включая запуск из подкаталога |
| Приложение | Корень, его непосредственные подкаталоги, `apps/*`, `crates/*`, `packages/*` |
| Профиль | Tauri при наличии `src-tauri/tauri.conf.json`; иначе Rust binary |
| Rust binary | `package.default-run` или единственный binary из Cargo; корневой пакет имеет приоритет перед вспомогательными Rust-пакетами |
| Имя файлов (`slug`) | Имя Rust binary; для Tauri — `package.name` из `src-tauri/Cargo.toml`; в нижнем регистре |
| Платформы | Linux x86_64, Windows x86_64, macOS aarch64 для standalone / Universal для Tauri |
| Frontend | `frontend/package.json`, затем `app/package.json` с `scripts.build`; для Tauri — `app/package.json` |
| Node.js | Совместная проверка `engines.node` в `package.json` и зависимостях из lock-файла; выбор из установленных версий в PATH, nvm или fnm |
| Rust toolchain | `stable` |
| Отправка тега | Выключена |

Если найдено несколько Tauri-приложений, несколько Rust-приложений без
корневого binary или несколько binary без `default-run`, CLI сообщает,
какой параметр нужно уточнить. Нестандартный путь задаётся через `app`.

Frontend выполняет `npm ci` (нужен `package-lock.json` или `npm-shrinkwrap.json`) и
`npm run build` один раз, перед тремя сборками Cargo с `--locked --release`.
Перед сборкой CLI читает `engines.node` из `package.json` приложения и его
родительских каталогов внутри Git, а также из ближайшего `package-lock.json`
(или `npm-shrinkwrap.json`, если он есть). Учитываются и devDependencies;
optional-пакеты для платформ, на которых npm не запускается, пропускаются.
Совместимый текущий Node.js сохраняется; иначе выбирается самая новая совместимая
установка из PATH, nvm или fnm. Все вызовы npm и их дочерние процессы получают
выбранный Node.js; Docker Tauri использует ту же точную версию. Если требования
не заданы, используется текущая установка без фиксированного минимума VerunaPack.
Если подходящей установки нет, CLI показывает проверенные версии и требования
зависимостей. Установите совместимый Node.js через nvm/fnm и повторите сборку.
Версия передаётся в `VITE_APP_VERSION`, `VERUNA_RELEASE_VERSION` и для standalone
в `<BINARY>_RELEASE_VERSION`: имя binary переводится в верхний регистр,
дефисы заменяются подчёркиваниями. Rust-приложение должно читать эту переменную
в build.rs и использовать одну константу версии в CLI/API; MockPort уже
выполняет этот контракт. Другое имя задаётся через `rust.version_env`.
Базовая версия берётся из Cargo, включая локальный `[workspace.package].version`.

MockPort работает без `release.toml`: постоянная идентичность и публичный ключ
берутся из существующего `release.config.json` рядом с Cargo.toml. Это данные
продукта для обновлений. Закрытый ключ: `<BINARY>_SIGNING_KEY` либо
`~/.<slug>-signing/publisher.private.pem`; пароль: `<BINARY>_KEY_PASSPHRASE`.

Для нестандартных параметров добавьте `release.toml` в корень Git.
Достаточно указать только отличия, например для «Моего Хоста»
([файл](examples/moy-host.release.toml)):

```toml
slug = "moy-host"
exclude = ["vpn-manager"]
[signing]
private_key_file = "~/.tauri/host-util.key"
```

Другие переопределения: `rust.features = ["feature"]`, `frontend.version_env`,
`frontend.enabled = false`, `targets`, `push_tag` и параметры подписи.
Таблицы дополняются по полям, списки заменяются целиком. Неизвестные поля и
ошибочный TOML останавливают запуск. Полные конфигурации прежнего формата
поддерживаются, включая проверку ранее созданных выпусков.
Другой файл: `veruna-pack --config path/settings.toml plan` — явно указанный
файл должен существовать. Пути внутри TOML отсчитываются от корня Git,
а `tauri.release_config` — от каталога приложения. Настройки для сборки должны
быть закоммичены вместе с исходниками.

Профиль сохраняет `identifier` и `plugins.updater.pubkey` из
`app/src-tauri/tauri.conf.json`. Закрытый ключ: `TAURI_SIGNING_PRIVATE_KEY` либо
`~/.tauri/<slug>.key`; пароль: `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
`src-tauri/tauri.release.conf.json` подключается автоматически, если существует;
без него достаточно основной конфигурации. Tauri использует свои платформенные
`tauri.macos.conf.json`/`tauri.linux.conf.json`, включая ASCII-имя исполняемого
файла macOS и настройки Debian «Моего Хоста». Frontend собирается один раз;
Tauri получает override с версией, включёнными bundle/updater artifacts и
пустым beforeBuildCommand. Версия передаётся также через VITE_APP_VERSION.
Изменения, которые сама Tauri вносит в Cargo-файлы, остаются во временном snapshot.
Манифесты, lock-файлы и исходники рабочего дерева продукта не переписываются.

## Правила версии и changelog

Основа — наибольший по SemVer стабильный достижимый тег `vX.Y.Z`.
Pre-release, build metadata и недостижимые теги не участвуют.
Диапазон — `<предыдущий-тег>..<исходный-SHA>`; без тегов — вся история,
а база берётся из манифеста приложения. Если HEAD уже имеет релизный тег,
используется его версия и диапазон от предыдущего релиза.

| Изменения в диапазоне | Повышение |
| --- | --- |
| `fix`, `perf`, `deps`, `revert` | patch |
| `feat` | minor |
| Conventional-заголовок с `!` или footer `BREAKING CHANGE:` / `BREAKING-CHANGE:` | minor до 1.0, major начиная с 1.0 |
| Только технические коммиты без breaking change | Нового выпуска нет |

Повышение применяется один раз, выбирается максимальное. Это правило первого
релиза одинаково для обоих профилей, даже если прежний скрипт продукта работал
иначе. Коммиты, меняющие только `exclude`, не учитываются. Смешанный коммит
учитывается целиком; имя каталога сравнивается по компонентам пути.
Обычные merge-сообщения и технические записи пропускаются; каждый Git-коммит
попадает в одну группу. Breaking change имеет отдельную группу с пояснением
из footer. Scope и исходный текст сохраняются.

План, CHANGELOG.md и текст аннотации тега используют одну строку Markdown,
включая заголовок и завершающий перевод строки. Локальный тег всегда указывает
на собранный SHA. Существующий тег на другом SHA — конфликт; перенос запрещён.
У заранее назначенного тега на том же SHA аннотация приводится к changelog
после успешной проверки. Совпадающий тег не изменяется при повторе.
По умолчанию `push_tag = false`. При `true` отправляется только
`refs/tags/v<версия>:refs/tags/v<версия>` в origin, без force и веток. `push.followTags` явно отключён для этого вызова.

Для shallow/partial clone, известных пропусков объектов и `--no-tags` выводится
инструкция получить историю. Локально невозможно обнаружить тег, который
никогда не был получен с сервера: перед выпуском полного clone выполняйте
`git fetch --tags origin`. План намеренно не требует сети.

## Ключи и совместимость обновлений

CLI использует существующие ключи продукта и не генерирует замену.
Переменная `private_key_env` имеет приоритет над `private_key_file` и может
содержать сам ключ или путь. Поддерживается `~` в локальных путях.
Пароль берётся из `password_env`; без него используется пустая строка,
интерактивного запроса нет. Ключи не записываются в комплект выпуска.

Standalone читает постоянный UUID `product_id` и публичный OKP/Ed25519 JWK
из `signing.identity` (`updates.public_key`). PKCS#8 PEM может быть зашифрован.
Сохраняется JWS Compact/EdDSA: typ `releases-standalone+jws`, RFC 7638 kid,
schema_version 1, application_id, version, os, arch, size_bytes и sha256.
macOS называется `macos` в имени файла и `darwin` в протоколе обновления.

Tauri читает base64 minisign secret key; публичный ключ берётся только из
доверенной конфигурации приложения. Перед сборкой проверяется пара ключей.
Подписи `.sig` берутся у Tauri и криптографически проверяются по окончательным
байтам. Для macOS updater — `.app.tar.gz`, для текущего «Моего Хоста» Linux
updater — DEB, Windows — NSIS EXE. Файл с двумя ролями сохраняется один раз.
Universal объявляет оба update target: `darwin-aarch64`, `darwin-x86_64`.
Это подписи обновлений; macOS publisher signing/notarization и Windows
Authenticode настраиваются отдельно средствами Tauri.

Закрытые переменные удаляются из окружения frontend и сторонних проверок;
Tauri получает свой ключ только на этапе упаковки. В Docker используются
BuildKit secrets с временными файлами mode 0600. Значения не становятся
build arguments, ENV образа или полями release.json.

## Готовый выпуск и восстановление

Выпуск требует чистых исходников, включая untracked-файлы. Каталог `releases/`
зарезервирован для результата и не должен содержать отслеживаемых файлов.
Его собственные выходы и исключённый независимый продукт не мешают повтору.
Можно дополнительно добавить `/releases/` в .gitignore для обычных команд Git.
Git submodules пока не поддерживаются.

Сборка использует временный Git snapshot зафиксированного SHA. Изменение HEAD,
исходников или влияющих на план тегов останавливает публикацию. OS-lock внутри
общего Git-каталога действует между worktree и освобождается ядром после сбоя.
Компиляторные cache находятся в общем Git-каталоге; исходники и bundle output
каждого запуска изолированы. Состав, версии пакетов, архитектуры, хеши и подписи
проверяются перед одним rename готового каталога в `releases/<версия>`.
Затем создаётся тег. Ошибка сборки/проверки не создаёт итоговый каталог и тег;
временный каталог сохраняется для диагностики, его путь выводится в ошибке.

В комплекте есть `CHANGELOG.md`, `release.json`, `SHA256SUMS`, три основных
дистрибутива и все фактические updater/signature-файлы. release.json содержит
версии продукта/CLI, SHA, оба тега, идентичность, fingerprint конфигурации,
OS/архитектуру/protocol target, роли, имя, размер, SHA-256 и имя подписи.
SHA256SUMS покрывает все остальные файлы, включая changelog, metadata и подписи.

После успешного `build` CLI выводит путь к выпуску, тег, исходный коммит и
примечания к релизу из того же changelog. Они выводятся и при повторном запуске
для уже готового выпуска.

Повторный `make release` проверяет готовый комплект публичным ключом, не требует
закрытого ключа и сборочных SDK. Если остался тег или его push, выполняется только
этот шаг. Конфликтующий или повреждённый выпуск сохраняется без перезаписи.
При конфликте CLI перечисляет отсутствующие и лишние файлы, показывает различия
полей `release.json` или ожидаемые и фактические размер/SHA-256 повреждённого
артефакта. Наличие `build.json` без `release.json` отдельно отмечается как
неполный комплект VerunaPack.

Для незавершённой локальной сборки ошибка предлагает команды: переместить каталог
в свободный `releases/.conflict-<версия>-<номер>` с `mv -n` и повторить `build`
с прежним `--config`, если он был указан. Это только подсказка: CLI сама ничего
не перемещает. Если выпуск уже опубликован, восстановите полный комплект из
доверенной копии и используйте исходный коммит и настройки этого выпуска;
новые изменения должны получить следующую версию.

Для отдельной проверки: `veruna-pack verify releases/<версия>` на исходном коммите.
Проверка DMG требует macOS. Она монтирует read-only образ и сравнивает приложение
с архивом updater; Linux/Windows проверяются по структуре ELF/DEB/PE и версиям.
Standalone запускает `version_args` нативного бинарника. У чужих бинарников
проверяется формат/архитектура и наличие версии; их runtime совместимость требует
отдельного запуска клиента на соответствующей ОС.

## Проверки

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Тесты транзакций используют временные Git-репозитории, настоящие ключи и bare
origin; сборочные файлы в них синтетические. Проверки продуктов и установки
обновления описаны отдельно в [отчёте приёмки](docs/acceptance.md).

Контракты сверены с [MockPort](https://github.com/averichev/mockport/blob/main/docs/releases.md),
[Моим Хостом](https://github.com/averichev/host-util/blob/main/apps/desktop/README.md#local-release-builds),
[Релизами](https://github.com/averichev/releases/blob/main/README.md#L138),
[standalone-updater](https://github.com/averichev/standalone-updater) и
[veruna-tauri-updates](https://github.com/averichev/veruna-tauri-updates).
