use crate::{
    artifacts::Spec,
    config::{Profile, Project, Target},
    plan::Plan,
    process,
};
use anyhow::{Context, Result, bail, ensure};
use flate2::read::GzDecoder;
use goblin::Object;
use std::{
    fs,
    io::{Cursor, Read},
    path::Path,
};

fn has_version(bytes: &[u8], version: &str) -> bool {
    bytes
        .windows(version.len())
        .any(|w| w == version.as_bytes())
}

pub fn executable(bytes: &[u8], target: &Target, standalone: bool) -> Result<()> {
    match (target.os.as_str(), Object::parse(bytes)?) {
        ("linux", Object::Elf(elf)) => {
            ensure!(
                elf.is_64 && elf.header.e_machine == goblin::elf::header::EM_X86_64,
                "Ожидается ELF64 x86_64"
            );
            ensure!(
                [goblin::elf::header::ET_EXEC, goblin::elf::header::ET_DYN]
                    .contains(&elf.header.e_type),
                "ELF не является исполняемым файлом"
            );
            if standalone {
                ensure!(
                    elf.interpreter.is_none() && elf.libraries.is_empty(),
                    "Standalone Linux должен быть статическим, без PT_INTERP/DT_NEEDED"
                );
            }
        }
        ("windows", Object::PE(pe)) => {
            ensure!(
                pe.is_64
                    && pe.header.coff_header.machine == goblin::pe::header::COFF_MACHINE_X86_64
                    && !pe.is_lib,
                "Ожидается PE AMD64 executable"
            );
            if standalone {
                const SYSTEM: &[&str] = &[
                    "advapi32.dll",
                    "bcrypt.dll",
                    "bcryptprimitives.dll",
                    "combase.dll",
                    "crypt32.dll",
                    "gdi32.dll",
                    "iphlpapi.dll",
                    "kernel32.dll",
                    "kernelbase.dll",
                    "msvcrt.dll",
                    "ntdll.dll",
                    "ole32.dll",
                    "oleaut32.dll",
                    "secur32.dll",
                    "shell32.dll",
                    "shlwapi.dll",
                    "user32.dll",
                    "userenv.dll",
                    "version.dll",
                    "winhttp.dll",
                    "winmm.dll",
                    "ws2_32.dll",
                    "dbghelp.dll",
                    "api-ms-win-core-synch-l1-2-0.dll",
                ];
                for library in pe.libraries {
                    ensure!(
                        SYSTEM.contains(&library.to_ascii_lowercase().as_str())
                            || library.to_ascii_lowercase().starts_with("api-ms-win-"),
                        "Несистемная DLL: {library}"
                    );
                }
            }
        }
        ("macos", Object::Mach(goblin::mach::Mach::Binary(mach))) => {
            ensure!(
                target.arch != "universal",
                "Ожидается Universal Mach-O, получен thin"
            );
            macho(&mach, &target.arch)?;
        }
        ("macos", Object::Mach(goblin::mach::Mach::Fat(fat))) => {
            ensure!(target.arch == "universal", "Неожиданный Universal Mach-O");
            let mut architectures = Vec::new();
            for index in 0..fat.narches {
                let goblin::mach::SingleArch::MachO(mach) = fat.get(index)? else {
                    bail!("Universal содержит не Mach-O");
                };
                let architecture = match mach.header.cputype {
                    goblin::mach::constants::cputype::CPU_TYPE_ARM64 => "aarch64",
                    goblin::mach::constants::cputype::CPU_TYPE_X86_64 => "x86_64",
                    _ => bail!("Неизвестная архитектура Mach-O"),
                };
                macho(&mach, architecture)?;
                architectures.push(architecture);
            }
            architectures.sort();
            ensure!(
                architectures == ["aarch64", "x86_64"],
                "Universal должен содержать ровно ARM64 и x86_64"
            );
        }
        _ => bail!("Неверный формат исполняемого файла для {}", target.os),
    }
    Ok(())
}

fn macho(mach: &goblin::mach::MachO<'_>, arch: &str) -> Result<()> {
    let expected = if arch == "aarch64" {
        goblin::mach::constants::cputype::CPU_TYPE_ARM64
    } else {
        goblin::mach::constants::cputype::CPU_TYPE_X86_64
    };
    ensure!(
        mach.is_64
            && mach.header.cputype == expected
            && mach.header.filetype == goblin::mach::header::MH_EXECUTE,
        "Неверная архитектура или тип Mach-O"
    );
    for lib in &mach.libs {
        ensure!(
            *lib == "self" || lib.starts_with("/usr/lib/") || lib.starts_with("/System/Library/"),
            "Несистемная dylib: {lib}"
        );
    }
    Ok(())
}

fn target(spec: &Spec) -> Target {
    Target {
        os: spec.os.clone(),
        arch: spec.arch.clone(),
    }
}

pub fn verify(project: &Project, plan: &Plan, directory: &Path, spec: &Spec) -> Result<()> {
    let file = directory.join(&spec.name);
    let version = plan.version.as_ref().unwrap().to_string();
    if project.config.profile == Profile::Standalone {
        let bytes = fs::read(&file)?;
        executable(&bytes, &target(spec), true)?;
        ensure!(
            has_version(&bytes, &version),
            "В бинарнике отсутствует рассчитанная версия {version}. Приложение должно читать rust.version_env в build.rs"
        );
        let native_os = std::env::consts::OS;
        if spec.os == native_os && spec.arch == std::env::consts::ARCH {
            let output = process::capture(
                process::command(project, &file, directory)
                    .args(&project.config.rust.as_ref().unwrap().version_args),
                "проверка версии нативного бинарника",
            )?;
            let words: Vec<_> = output
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
                .collect();
            ensure!(
                words.contains(&version.as_str()),
                "Бинарник сообщает другую версию: {output}"
            );
        }
        return Ok(());
    }
    match spec.os.as_str() {
        "macos" if spec.name.ends_with(".app.tar.gz") => {
            let (plist, binary) = app_archive(&fs::read(&file)?)?;
            check_plist(project, &version, &plist)?;
            executable(&binary, &target(spec), false)?;
        }
        "macos" => verify_dmg(project, plan, directory, spec)?,
        "linux" => verify_deb(&fs::read(&file)?, &version, spec)?,
        "windows" => {
            let bytes = fs::read(file)?;
            // Packed NSIS stubs may have stripped relocation tables. Parse only
            // their header; the extracted application is validated separately.
            let header = goblin::pe::header::Header::parse(&bytes)?;
            ensure!(
                matches!(
                    header.coff_header.machine,
                    goblin::pe::header::COFF_MACHINE_X86 | goblin::pe::header::COFF_MACHINE_X86_64
                ),
                "NSIS installer должен быть PE x86/x64"
            );
            let flags = header.coff_header.characteristics;
            ensure!(
                goblin::pe::characteristic::is_exe(flags)
                    && !goblin::pe::characteristic::is_dll(flags),
                "Installer должен быть исполняемым PE, а не DLL"
            );
            ensure!(
                windows_version(&bytes, &version)?,
                "Версия Windows installer не соответствует {version}"
            );
            verify_nsis_payload(project, directory, spec)?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

pub fn check_plist(project: &Project, version: &str, bytes: &[u8]) -> Result<plist::Value> {
    let value = plist::Value::from_reader(Cursor::new(bytes))?;
    let dict = value
        .as_dictionary()
        .context("Info.plist не является dictionary")?;
    ensure!(
        dict.get("CFBundleShortVersionString")
            .and_then(|v| v.as_string())
            == Some(version),
        "Версия .app не соответствует {version}"
    );
    ensure!(
        dict.get("CFBundleVersion").and_then(|v| v.as_string()) == Some(version),
        "CFBundleVersion не соответствует {version}"
    );
    ensure!(
        dict.get("CFBundleIdentifier").and_then(|v| v.as_string()) == Some(project.identifier()),
        "Изменился идентификатор Tauri приложения"
    );
    Ok(value)
}

fn app_archive(bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut archive = tar::Archive::new(GzDecoder::new(bytes));
    let mut info = None;
    let mut binaries = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !entry.header().entry_type().is_file() {
            continue;
        }
        if path.ends_with("Contents/Info.plist") {
            ensure!(info.is_none(), "В updater несколько .app");
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            info = Some(bytes);
        } else if path.parent().is_some_and(|p| p.ends_with("Contents/MacOS")) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            binaries.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                bytes,
            ));
        }
    }
    let info = info.context("Нет Info.plist в updater .app.tar.gz")?;
    let value = plist::Value::from_reader(Cursor::new(&info))?;
    let executable = value
        .as_dictionary()
        .and_then(|d| d.get("CFBundleExecutable"))
        .and_then(|s| s.as_string())
        .context("Нет CFBundleExecutable")?;
    let binary = binaries
        .into_iter()
        .find(|(name, _)| name == executable)
        .context("Нет исполняемого файла .app")?
        .1;
    Ok((info, binary))
}

fn verify_dmg(project: &Project, plan: &Plan, directory: &Path, spec: &Spec) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "Проверка DMG требует macOS; повторите veruna-pack build на сборочном Mac"
    );
    let mount = tempfile::tempdir()?;
    let file = directory.join(&spec.name);
    process::capture(
        process::command(project, "hdiutil", directory)
            .args([
                "attach",
                "-readonly",
                "-nobrowse",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(mount.path())
            .arg(&file),
        "монтирование DMG",
    )?;
    let result = (|| -> Result<()> {
        let apps: Vec<_> = fs::read_dir(mount.path())?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "app"))
            .collect();
        ensure!(apps.len() == 1, "В DMG требуется ровно одна .app");
        let info = fs::read(apps[0].join("Contents/Info.plist"))?;
        let value = check_plist(project, &plan.version.as_ref().unwrap().to_string(), &info)?;
        let executable = value.as_dictionary().unwrap()["CFBundleExecutable"]
            .as_string()
            .context("Нет CFBundleExecutable")?;
        let binary = fs::read(apps[0].join("Contents/MacOS").join(executable))?;
        executable_bytes_match(directory, spec, &binary)?;
        executable_check(&binary, spec)?;
        Ok(())
    })();
    let detach = process::capture(
        process::command(project, "hdiutil", directory)
            .arg("detach")
            .arg(mount.path()),
        "отключение DMG",
    );
    result?;
    detach?;
    Ok(())
}
fn executable_check(binary: &[u8], spec: &Spec) -> Result<()> {
    executable(binary, &target(spec), false)
}
fn executable_bytes_match(directory: &Path, spec: &Spec, binary: &[u8]) -> Result<()> {
    let updater = spec.name.strip_suffix(".dmg").unwrap().to_owned() + ".app.tar.gz";
    let (_, archived) = app_archive(&fs::read(directory.join(updater))?)?;
    ensure!(
        binary == archived,
        "В DMG и updater разные исполняемые файлы"
    );
    Ok(())
}

fn untar_control(bytes: &[u8]) -> Result<String> {
    let mut tar = tar::Archive::new(Cursor::new(bytes));
    for entry in tar.entries()? {
        let mut entry = entry?;
        if entry.path()?.file_name().is_some_and(|n| n == "control") {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            return Ok(text);
        }
    }
    bail!("Нет control в Debian package")
}
fn decompress(name: &str, data: &[u8]) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    if name.ends_with(".gz") {
        GzDecoder::new(data).read_to_end(&mut result)?;
    } else if name.ends_with(".xz") {
        xz2::read::XzDecoder::new(data).read_to_end(&mut result)?;
    } else if name.ends_with(".zst") {
        result = zstd::stream::decode_all(data)?;
    } else if name.ends_with(".tar") {
        result.extend_from_slice(data);
    } else {
        bail!("Неподдерживаемое сжатие DEB: {name}");
    }
    Ok(result)
}

fn verify_deb(bytes: &[u8], version: &str, spec: &Spec) -> Result<()> {
    ensure!(
        bytes.starts_with(b"!<arch>\n"),
        "Ожидается Debian ar archive"
    );
    let mut offset = 8;
    let mut control = None;
    let mut data = None;
    while offset + 60 <= bytes.len() {
        let header = &bytes[offset..offset + 60];
        ensure!(&header[58..60] == b"`\n", "Повреждён DEB ar header");
        let name = std::str::from_utf8(&header[..16])?
            .trim()
            .trim_end_matches('/');
        let size: usize = std::str::from_utf8(&header[48..58])?.trim().parse()?;
        offset += 60;
        let end = offset.checked_add(size).context("DEB size overflow")?;
        ensure!(end <= bytes.len(), "Повреждён DEB member");
        if name.starts_with("control.tar") {
            ensure!(control.is_none(), "Два control archive");
            control = Some(untar_control(&decompress(name, &bytes[offset..end])?)?);
        }
        if name.starts_with("data.tar") {
            ensure!(data.is_none(), "Два data archive");
            data = Some(decompress(name, &bytes[offset..end])?);
        }
        offset = end + size % 2;
    }
    let control = control.context("Нет Debian control archive")?;
    ensure!(
        control.lines().any(|s| s == format!("Version: {version}")),
        "Неверная версия Debian package"
    );
    ensure!(
        control.lines().any(|s| s == "Architecture: amd64"),
        "Неверная архитектура Debian package"
    );
    let mut archive = tar::Archive::new(Cursor::new(data.context("Нет Debian data archive")?));
    let mut count = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_file()
            && entry
                .path()?
                .parent()
                .is_some_and(|p| p.ends_with("usr/bin"))
        {
            let mut binary = Vec::new();
            entry.read_to_end(&mut binary)?;
            executable(&binary, &target(spec), false)?;
            count += 1;
        }
    }
    ensure!(count > 0, "DEB не содержит исполняемый файл приложения");
    Ok(())
}

fn windows_version(bytes: &[u8], version: &str) -> Result<bool> {
    let version = semver::Version::parse(version)?;
    let expected_ms = (version.major << 16) | version.minor;
    let expected_ls = version.patch << 16;
    // VS_FIXEDFILEINFO uses little endian fields inside the PE version resource.
    for (offset, window) in bytes.windows(4).enumerate() {
        if window == [0xbd, 0x04, 0xef, 0xfe] && offset + 16 <= bytes.len() {
            let ms = u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into()?) as u64;
            let ls = u32::from_le_bytes(bytes[offset + 12..offset + 16].try_into()?) as u64;
            if ms == expected_ms && ls == expected_ls {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn verify_nsis_payload(project: &Project, directory: &Path, spec: &Spec) -> Result<()> {
    let work = tempfile::tempdir()?;
    process::capture(
        process::command(project, "7zz", directory)
            .args(["x", "-y", "-bd"])
            .arg(format!("-o{}", work.path().display()))
            .arg(directory.join(&spec.name)),
        "извлечение NSIS для проверки архитектуры",
    )?;
    let name = project.identity["mainBinaryName"]
        .as_str()
        .or_else(|| project.identity["productName"].as_str())
        .context("Нет имени исполняемого файла Tauri")?;
    let expected = format!("{name}.exe");
    let mut matches = Vec::new();
    fn walk(path: &Path, expected: &str, matches: &mut Vec<std::path::PathBuf>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                walk(&entry.path(), expected, matches)?;
            } else if entry.file_type()?.is_file() && entry.file_name() == expected {
                matches.push(entry.path());
            }
        }
        Ok(())
    }
    walk(work.path(), &expected, &mut matches)?;
    ensure!(
        matches.len() == 1,
        "NSIS должен содержать ровно один {expected}"
    );
    executable(&fs::read(&matches[0])?, &target(spec), false)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_binary_is_inspected_and_wrong_architecture_is_rejected() {
        let bytes = fs::read(std::env::current_exe().unwrap()).unwrap();
        let target = Target {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
        };
        executable(&bytes, &target, false).unwrap();
        let mut wrong = target;
        wrong.os = if wrong.os == "windows" {
            "linux".into()
        } else {
            "windows".into()
        };
        assert!(executable(&bytes, &wrong, false).is_err());
        assert!(executable(b"not an executable", &wrong, false).is_err());
    }
}
