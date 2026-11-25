# Приёмка veruna-pack

Дата: 2026-11-24. Сборочная машина: Apple Silicon macOS; Docker Desktop Linux VM.
Проверки продуктов выполняются только в `/tmp/veruna-pack-validation/`.
Рабочие копии продуктов не изменялись. Ключи тестовые, находятся вне checkout.

## Установка

```sh
CARGO_TARGET_DIR=/tmp/veruna-pack-validation/cli-target cargo install --locked --path . --root /tmp/veruna-pack-validation/cli
export PATH="/tmp/veruna-pack-validation/cli/bin:$PATH"
```

Установлен один `veruna-pack` 0.1.0. `otool -L` показывает только системные
`libSystem` и `libiconv`; liblzma и zstd встроены.

## Автоматические проверки

```sh
cargo fmt --check
CARGO_TARGET_DIR=/tmp/veruna-pack-validation/cli-target cargo clippy --locked --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/veruna-pack-validation/cli-target cargo test --locked
```

Все три команды прошли; 23 теста успешно.

Проверены расчёт версии/первый релиз, breaking до и после 1.0, техническая
история, SemVer-порядок, tagged HEAD, исключённый продукт, shallow/promisor
clone, JWS с неверным ключом и полями, криптографическая проверка Tauri,
отказ сборки, конфликт тега, изменение исходников, OS-lock, публикация без
перезаписи, повреждение комплекта и восстановление после отказа push, включая `push.followTags=true`
и аннотированный предыдущий тег: в bare origin ушёл только тег выпуска.
Транзакционные тесты используют синтетические байты, настоящий Git и подписи;
они не заменяют проверки продуктов ниже.

## Полный выпуск минимального Rust-приложения

Реальные Cargo/Zig/MinGW/Apple SDK, обычный `make release`, без release-скриптов.
Предыдущий тег `v0.2.4`; коммиты:

```text
feat(search): поиск по ИНН
fix(updater): повтор загрузки после обрыва
chore(ci): обновить кеш
```

План дал `v0.3.0`, две пользовательские записи, исходный SHA `d0b9f2acd177e58c52719298c2049a7d26e1c0dd`.
Собраны и проверены ELF64 static x86_64, PE AMD64 и Mach-O ARM64.
План, CHANGELOG.md и тело аннотированного тега совпали побайтово.
Дополнительная реальная сборка в `sample-fail` завершилась на Windows с
`compile_error!("injected Windows build failure")` после успешной Linux-цели.
`releases/0.3.1` и `v0.3.1` не созданы, `.work-DxowKb` сохранён.

Повтор с PATH `/usr/bin:/bin` и заведомо отсутствующим приватным ключом
сохранил объект тега и SHA256SUMS, без вызова сборочных инструментов.

Команды:

```sh
cd /tmp/veruna-pack-validation/sample
make release-plan
make release
veruna-pack plan --json
veruna-pack verify releases/0.3.0
PATH=/usr/bin:/bin SAMPLE_PRIVATE_KEY=/definitely/missing/key /tmp/veruna-pack-validation/cli/bin/veruna-pack build
```

Дерево готового выпуска:

```text
releases/0.3.0/
  CHANGELOG.md
  SHA256SUMS
  release.json
  sample-0.3.0-linux-x86_64
  sample-0.3.0-linux-x86_64.jws
  sample-0.3.0-macos-aarch64.tool
  sample-0.3.0-macos-aarch64.tool.jws
  sample-0.3.0-windows-x86_64.exe
  sample-0.3.0-windows-x86_64.exe.jws
```

Фактический changelog:

```markdown
# Версия 0.3.0

## Новое

- **search:** поиск по ИНН

## Исправления

- **updater:** повтор загрузки после обрыва
```

## Воспроизведение на копиях продуктов

Для нового прогона используйте новый каталог. Скрипт ниже клонирует исходники,
создаёт одну тестовую пару ключей на продукт, закрепляет её в обеих версиях,
меняет только тестовые endpoints на loopback и сохраняет UUID/identifier.
Тестовая база `0.50.0` выбрана отдельно от публичных версий продуктов.
В `*-next` добавляются две цели Makefile; старые release-скрипты удалены.
Текущая версия скрипта оставляет MockPort без `release.toml`, а для «Моего Хоста»
записывает только нестандартные настройки. Скрипт печатает команды legacy и новых сборок.

```sh
python3 scripts/prepare-product-copies.py /tmp/veruna-pack-rehearsal
```

В выполненном прогоне legacy MockPort 0.50.0 собран на трёх платформах старым
`make release`; нативный smoke прошёл. Legacy «Мой Хост» 0.50.0 собран старым
`make release-macos`: Universal DMG, .app.tar.gz и .sig; установленная тестовая
копия .app прошла `codesign --verify --deep --strict`.

### MockPort: полный выпуск и реальная установка

`make release` в `mockport-next` создал проверенный выпуск 0.51.0 и аннотированный
тег v0.51.0 на SHA `e4a38be3f900ecfdba26ab7a1d6f62e468e942d3`.
Frontend собран один раз на запуск, старые release/build-скрипты удалены.
План, файл changelog и тело тега совпали побайтово. Повтор без ключа, npm,
Cargo и SDK-команд в PATH сохранил объект тега и SHA256SUMS.

```text
releases/0.51.0/
  CHANGELOG.md
  SHA256SUMS
  mockport-0.51.0-linux-x86_64
  mockport-0.51.0-linux-x86_64.jws
  mockport-0.51.0-macos-aarch64.tool
  mockport-0.51.0-macos-aarch64.tool.jws
  mockport-0.51.0-windows-x86_64.exe
  mockport-0.51.0-windows-x86_64.exe.jws
  release.json
```

```markdown
# Версия 0.51.0

## Новое

- **release:** проверить выпуск через veruna-pack
```

Реальная установка старого бинарника 0.50.0, собранного прежним способом,
на новый 0.51.0 проверена через API MockPort и production standalone-updater
в `python:3.12-alpine`, linux/amd64, с `--network none`. Сервер обновлений
работал на loopback внутри контейнера. Клиент подтвердил подпись,
завершил замену процесса, /health сообщил 0.51.0, SHA-256 установленного
файла совпал с новым выпуском. Тестовая SQLite и сохранённая запись остались.

```sh
MOCKPORT_SIGNING_KEY=/tmp/veruna-pack-validation/test-keys/mockport.pem \
  make -C /tmp/veruna-pack-validation/mockport-next release
python3 scripts/check-mockport-update.py \
  --old /tmp/veruna-pack-validation/mockport/dist/signed/0.50.0/mockport-0.50.0-linux-x86_64 \
  --new /tmp/veruna-pack-validation/mockport-next/releases/0.51.0/mockport-0.51.0-linux-x86_64 \
  --jws /tmp/veruna-pack-validation/mockport-next/releases/0.51.0/mockport-0.51.0-linux-x86_64.jws \
  --old-version 0.50.0 --new-version 0.51.0
```

Результат клиента:

```json
{"result": "passed", "client": "MockPort/standalone-updater", "old_version": "0.50.0", "new_version": "0.51.0", "installed_sha256": "829270afd2b9adb8d2733d80af15bc90694b790fb2ed05128199280bf9ecff69", "sqlite": "preserved"}
```

Проверены также главная SPA-страница и её JS-ресурсы после обновления.
На первом проходе исправлен список системных DLL Windows после проверки
реальных импортов через objdump. Не прошедший проверку комплект не публиковался.

### Мой Хост

Legacy macOS Universal 0.50.0 готов: DMG, .app.tar.gz и .sig.
Исходный коммит новой версии: `404edb094ce9dc0ee61c2722ee61d0cc2b18790d`.
В новой копии удалены `scripts/release.py`, `scripts/build_macos.py`,
`scripts/collect_release.py` и `Dockerfile.release`. Команда выпуска:

```sh
TAURI_SIGNING_PRIVATE_KEY=/tmp/veruna-pack-validation/test-keys/host-util.key \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD='' \
  make -C /tmp/veruna-pack-validation/host-next release
```

Полный выпуск 0.51.0 успешно создан этой командой: macOS Universal DMG и
`.app.tar.gz` + `.sig`, Linux amd64 DEB + `.sig`, Windows x64 NSIS EXE + `.sig`.
DEB и NSIS имеют одновременно роли installer/updater и лежат по одному разу.
Все три подписи проверены закреплённым ключом приложения; DMG сопоставлен
с updater, версии и архитектуры проверены, включая PE AMD64 внутри NSIS.
Каталог опубликован одной операцией, затем создан аннотированный `v0.51.0`
на собранном SHA. macOS пакеты используют ad-hoc codesign, без notarization;
это проверка updater-ключа, а не издательского сертификата Apple.

План, CHANGELOG.md и аннотация совпали побайтово. Повтор с заведомо
отсутствующим ключом сохранил объект тега
`d888e0170fafccf2bfc7a0ea4dcc31fad9632b8f` и SHA-256 файла SHA256SUMS
`b16bd7f3ee4d05a1c805662b2d9bfe501f486ab84459467d5e558d069d9cfcd4`.
Сборка при повторе не запускалась; использованы публичный ключ, hdiutil и 7zz.

```sh
cd /tmp/veruna-pack-validation/host-next
PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin \
TAURI_SIGNING_PRIVATE_KEY=/definitely/missing/key \
  /tmp/veruna-pack-validation/cli/bin/veruna-pack build
```

```text
releases/0.51.0/
  CHANGELOG.md
  SHA256SUMS
  release.json
  moy-host-0.51.0-linux-x86_64.deb
  moy-host-0.51.0-linux-x86_64.deb.sig
  moy-host-0.51.0-macos-universal.dmg
  moy-host-0.51.0-macos-universal.app.tar.gz
  moy-host-0.51.0-macos-universal.app.tar.gz.sig
  moy-host-0.51.0-windows-x86_64-setup.exe
  moy-host-0.51.0-windows-x86_64-setup.exe.sig
```

При проверке реальной сборки устранены три проблемы: загрузка SDK Microsoft
через поддерживаемый native-tls backend cargo-xwin; UTF-8 локаль NSIS для
кириллицы в имени приложения; проверка заголовка упакованного загрузчика NSIS
с отдельной проверкой архитектуры извлечённого приложения. TLS-проверка
сертификатов не отключается. Все прежние неудачные `.work-*` сохранены;
на их основании итоговый каталог и тег не создавались.

### Установка обновления «Моего Хоста»: блокер

Реальная установка через штатный интерфейс **не проверена**: Mac заблокирован,
computer-use не смог открыть приложение и потребовал ручной unlock.
Запрос разблокировать экран передан пользователю. Старый тестовый клиент
запущен с отдельными `HOME`/`CFFIXED_USER_HOME`; SQLite и WebKit находятся
в `/tmp/veruna-pack-validation/host-home`, вне пользовательского профиля.

После ручного unlock точное продолжение проверки:

```sh
python3 scripts/serve-tauri-update.py \
  --package /tmp/veruna-pack-validation/host-next/releases/0.51.0/moy-host-0.51.0-macos-universal.app.tar.gz \
  --signature /tmp/veruna-pack-validation/host-next/releases/0.51.0/moy-host-0.51.0-macos-universal.app.tar.gz.sig \
  --version 0.51.0
```

В тестовой `/tmp/veruna-pack-validation/installed/Мой хост.app` открыть
«Настройки» → «Проверить обновления» → «Установить и перезапустить».
Проверить `CFBundleShortVersionString=0.51.0`, SHA-256 бинарника против
нового `.app.tar.gz`, сохранность тестовой SQLite и запуск новой версии.
Для нового запуска копии использовать изолированный профиль:

```sh
HOME=/tmp/veruna-pack-validation/host-home \
CFFIXED_USER_HOME=/tmp/veruna-pack-validation/host-home \
XDG_CONFIG_HOME=/tmp/veruna-pack-validation/host-home/config \
XDG_DATA_HOME=/tmp/veruna-pack-validation/host-home/data \
  '/tmp/veruna-pack-validation/installed/Мой хост.app/Contents/MacOS/moy-host'
```

## Удалённый репозиторий

Создан локальный самостоятельный Git-репозиторий с origin
`git@github.com:averichev/veruna-pack.git`. На момент проверки удалённый
репозиторий возвращает `Repository not found` по SSH и недоступен по API.
GitHub CLI не авторизован; браузер GitHub открывает форму входа. Публикация
в GitHub **не выполнена**. Нужен вход в GitHub или создание пустого репозитория
`averichev/veruna-pack`; после этого из каталога утилиты:

```sh
git push -u origin main
```

## Необязательный release.toml — 2026-11-25

`make check` прошёл: rustfmt, Clippy с `-D warnings`, 32 теста.
Покрыты определение Rust/Tauri-приложения, запуск из подкаталога, отсутствие
конфига, частичные переопределения, неизвестные поля, неоднозначные приложения
и binary, исключения и отключение frontend. Явный `--config` с отсутствующим
файлом остаётся ошибкой. Fingerprint старых полных конфигураций сохранён.

Установленная CLI из fish выполнила `plan --json` в рабочей копии MockPort без
`release.toml`: версия `0.9.0`, Linux x86_64, Windows x86_64, macOS aarch64.
Рабочая копия продукта не изменялась.

В `/tmp/veruna-pack-defaults-w6ogfass/project` создано отдельное минимальное
Rust-приложение с постоянной идентичностью в `release.config.json` и тестовым
ключом вне Git. `release.toml` отсутствует на всех этапах. Настоящие Cargo,
Zig, MinGW и Apple SDK собрали выпуск `0.2.0` на трёх платформах; CLI проверила
бинарники и JWS, сохранила комплект и аннотированный тег `v0.2.0`.
Исходные Cargo-файлы остались с версией `0.1.0`.

`verify releases/0.2.0` и повторный `build` прошли с PATH `/usr/bin:/bin`
и заведомо отсутствующим закрытым ключом. Объект тега и SHA256SUMS не изменились.
SHA-256 файла SHA256SUMS:
`e0b356062d88cbd5df61b46051dce8f592a9605de7505286865f85f135e5d5cd`.

Полный прогон выявил и подтвердил исправления выбора rustc из указанного
toolchain при Homebrew Rust в PATH и проверки выпуска по относительному пути.
Синтаксис Python-скрипта подготовки копий и shell-команд Dockerfile проверен.
Полная повторная сборка Tauri в рамках этого изменения не выполнялась.
