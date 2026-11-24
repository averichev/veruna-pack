# veruna-pack

Одна Rust CLI для локального выпуска приложений с Mac: версия из Git,
frontend, три платформы, подписи обновлений, changelog и аннотированный тег.
Профили вызывают инструменты сборки непосредственно; старые release-скрипты
продукта и его Dockerfile выпуска не нужны.

## Установка и обновление

На сборочном Mac нужны Git с полной историей и тегами, make, rustup/Rust 1.91+
с rustfmt/Clippy, Xcode Command Line Tools и Node.js 22.12+/npm.
Установка самой CLI также требует системный C toolchain (зависимости сжатия).

```sh
git clone git@github.com:averichev/veruna-pack.git
cd veruna-pack
cargo install --locked --path . --root "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
veruna-pack --version
```

Добавьте изменение PATH в `~/.zshrc`. Для обновления в копии **утилиты**:

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
описание этих сборок (Rust 1.91.1, Node 22); в продукт его копировать не нужно.
Версию Rust контейнера можно задать через `tauri.docker_rust_version`.
Первое заполнение SDK, образов, npm и Cargo cache требует сети и места на диске.
Microsoft SDK используется на условиях его лицензии, как в прежнем процессе.
Продукт должен фиксировать Tauri CLI в package-lock.json.

## Подключение проекта

Скопируйте подходящий пример в **корень Git-репозитория** как `release.toml`
и добавьте две цели в Makefile:

```makefile
.PHONY: release release-plan

release:
	veruna-pack build

release-plan:
	veruna-pack plan
```

Закоммитьте конфигурацию и Makefile. `make release-plan` читает проект и Git,
не открывает закрытых ключей, не собирает приложение и не создаёт файлов.
`veruna-pack plan --json` возвращает те же данные с полем `changelog`.
Другой путь к конфигурации: `veruna-pack --config path/release.toml plan`.
Все относительные пути внутри TOML отсчитываются от корня Git.

Рабочий конфиг MockPort ([файл](examples/mockport.release.toml)):

```toml
slug = "mockport"
profile = "standalone"
app = "."
push_tag = false
targets = [
    { os = "linux", arch = "x86_64" },
    { os = "windows", arch = "x86_64" },
    { os = "macos", arch = "aarch64" },
]
[rust]
binary = "mockport"
version_env = "MOCKPORT_RELEASE_VERSION"
toolchain = "stable"
version_args = ["--version"]
[frontend]
path = "frontend"
version_env = "VITE_APP_VERSION"
[signing]
identity = "release.config.json"
private_key_env = "MOCKPORT_SIGNING_KEY"
private_key_file = "~/.mockport-signing/publisher.private.pem"
password_env = "MOCKPORT_KEY_PASSPHRASE"
```

`[frontend]` необязателен для standalone. Он выполняет `npm ci` и
`npm run build` один раз, перед тремя сборками Cargo с `--locked --release`.
Rust-приложение должно читать `rust.version_env` в build.rs и использовать
одну константу версии в CLI/API; MockPort уже выполняет этот контракт.
Для других приложений это часть интеграции версии, а не функция Cargo.
Поддерживается `rust.features = ["feature"]`. Cargo-версия берётся из
`app/Cargo.toml`, включая локальный `[workspace.package].version`.

Рабочий конфиг «Моего Хоста» ([файл](examples/moy-host.release.toml)):

```toml
slug = "moy-host"
profile = "tauri"
app = "apps/desktop"
exclude = ["vpn-manager"]
push_tag = false
targets = [
    { os = "macos", arch = "universal" },
    { os = "linux", arch = "x86_64" },
    { os = "windows", arch = "x86_64" },
]
[frontend]
path = "apps/desktop"
version_env = "VITE_APP_VERSION"
[tauri]
release_config = "src-tauri/tauri.release.conf.json"
toolchain = "stable"
[signing]
private_key_env = "TAURI_SIGNING_PRIVATE_KEY"
private_key_file = "~/.tauri/host-util.key"
password_env = "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"
```

Профиль сохраняет `identifier` и `plugins.updater.pubkey` из
`app/src-tauri/tauri.conf.json`. Tauri использует свои платформенные
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

Повторный `make release` проверяет готовый комплект публичным ключом, не требует
закрытого ключа и сборочных SDK. Если остался тег или его push, выполняется только
этот шаг. Конфликтующий или повреждённый выпуск сохраняется без перезаписи.
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
