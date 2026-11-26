use crate::{
    artifacts::Artifact,
    config::{Profile, Project},
};
use anyhow::{Context, Result, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey, pkcs8::DecodePrivateKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::Cursor,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Jwk {
    kty: String,
    crv: String,
    x: String,
}

fn jwk(project: &Project) -> Result<(VerifyingKey, String)> {
    let source = project.identity.get("updates").unwrap_or(&project.identity);
    let key = source
        .get("public_key")
        .context("В signing.identity отсутствует public_key")?;
    let jwk: Jwk = serde_json::from_value(key.clone())?;
    ensure!(
        jwk.kty == "OKP" && jwk.crv == "Ed25519",
        "Ожидается публичный OKP/Ed25519 JWK"
    );
    let bytes = URL_SAFE_NO_PAD.decode(&jwk.x)?;
    ensure!(
        bytes.len() == 32 && URL_SAFE_NO_PAD.encode(&bytes) == jwk.x,
        "Неканонический публичный ключ Ed25519"
    );
    let key = VerifyingKey::from_bytes(bytes.as_slice().try_into()?)?;
    let canonical = format!(
        "{{\"crv\":\"Ed25519\",\"kty\":\"OKP\",\"x\":\"{}\"}}",
        jwk.x
    );
    let kid = URL_SAFE_NO_PAD.encode(Sha256::digest(canonical));
    Ok((key, kid))
}

pub fn expand(path: &Path) -> Result<PathBuf> {
    if let Ok(rest) = path.strip_prefix("~") {
        return Ok(PathBuf::from(env::var_os("HOME").context("Не задан HOME")?).join(rest));
    }
    Ok(path.to_owned())
}

fn secret(project: &Project) -> Result<Zeroizing<String>> {
    let settings = &project.config.signing;
    let name = &settings.private_key_env;
    let value = match env::var(name) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => {
            let file = settings.private_key_file.as_deref().with_context(|| {
                format!("Укажите путь к закрытому ключу в переменной {name} или в signing.private_key_file")
            })?;
            let path = project.root.join(expand(file)?);
            return fs::read_to_string(&path).map(Zeroizing::new).map_err(|error| {
                anyhow::anyhow!(
                    "Не удалось прочитать файл закрытого ключа «{}» ({error}).\n\
                     Укажите путь к существующему ключу в переменной {name} или в signing.private_key_file.",
                    path.display()
                )
            });
        }
        Err(_) => anyhow::bail!("Значение {name} должно быть в UTF-8"),
    };
    let value = Zeroizing::new(value);
    if value.starts_with("-----BEGIN ")
        || STANDARD
            .decode(value.trim())
            .is_ok_and(|b| b.starts_with(b"untrusted comment:"))
    {
        return Ok(value);
    }
    let path = project.root.join(expand(Path::new(value.as_str()))?);
    // The environment value may be malformed key material. Keep it out of diagnostics.
    fs::read_to_string(path).map(Zeroizing::new).map_err(|_| {
        anyhow::anyhow!(
            "В переменной {name} укажите закрытый ключ или путь к доступному файлу ключа."
        )
    })
}

pub enum Material {
    Standalone(Box<SigningKey>),
    Tauri {
        encoded: Zeroizing<String>,
        password: Zeroizing<String>,
    },
}

impl Material {
    pub fn load(project: &Project) -> Result<Self> {
        let value = secret(project)?;
        let password = Zeroizing::new(
            project
                .config
                .signing
                .password_env
                .as_ref()
                .and_then(|name| env::var(name).ok())
                .unwrap_or_default(),
        );
        match project.config.profile {
            Profile::Standalone => {
                let key = if value.starts_with("-----BEGIN ENCRYPTED PRIVATE KEY-----") {
                    SigningKey::from_pkcs8_encrypted_pem(&value, password.as_bytes())
                } else {
                    SigningKey::from_pkcs8_pem(&value)
                }
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Не удалось открыть PKCS#8 Ed25519 ключ: проверьте формат и пароль"
                    )
                })?;
                let (trusted, _) = jwk(project)?;
                ensure!(
                    key.verifying_key() == trusted,
                    "Закрытый ключ не соответствует публичному ключу приложения"
                );
                Ok(Self::Standalone(Box::new(key)))
            }
            Profile::Tauri => {
                let decoded = Zeroizing::new(
                    STANDARD
                        .decode(value.trim())
                        .map_err(|_| anyhow::anyhow!("Ожидается Tauri base64 secret key"))?,
                );
                let text = std::str::from_utf8(&decoded)
                    .map_err(|_| anyhow::anyhow!("Неверный формат закрытого ключа Tauri"))?;
                let secret = minisign::SecretKeyBox::from_string(text)
                    .and_then(|b| b.into_secret_key(Some(password.to_string())))
                    .map_err(|_| {
                        anyhow::anyhow!("Не удалось открыть Tauri ключ: проверьте формат и пароль")
                    })?;
                let public = tauri_public(project)?;
                let challenge = b"veruna-pack signing-key preflight";
                let signature = minisign::sign(
                    Some(&public),
                    &secret,
                    Cursor::new(challenge),
                    Some("veruna-pack key check"),
                    None,
                )
                .map_err(|_| {
                    anyhow::anyhow!("Закрытый ключ Tauri не соответствует ключу приложения")
                })?;
                minisign::verify(
                    &public,
                    &signature,
                    Cursor::new(challenge),
                    true,
                    false,
                    false,
                )
                .map_err(|_| {
                    anyhow::anyhow!("Закрытый ключ Tauri не соответствует ключу приложения")
                })?;
                Ok(Self::Tauri {
                    encoded: value,
                    password,
                })
            }
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Payload {
    schema_version: u8,
    application_id: String,
    version: String,
    os: String,
    arch: String,
    size_bytes: u64,
    sha256: String,
}

fn payload(project: &Project, version: &str, artifact: &Artifact) -> Payload {
    Payload {
        schema_version: 1,
        application_id: project.identifier().into(),
        version: version.into(),
        os: artifact.protocol_os.clone(),
        arch: artifact.arch.clone(),
        size_bytes: artifact.size_bytes,
        sha256: artifact.sha256.clone(),
    }
}

pub fn sign_jws(
    project: &Project,
    version: &str,
    artifact: &Artifact,
    key: &SigningKey,
) -> Result<String> {
    let (_, kid) = jwk(project)?;
    let header = Header {
        alg: "EdDSA".into(),
        typ: "releases-standalone+jws".into(),
        kid,
    };
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header)?),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload(project, version, artifact))?)
    );
    Ok(format!(
        "{input}.{}\n",
        URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()).to_bytes())
    ))
}

pub fn verify_jws(
    project: &Project,
    version: &str,
    artifact: &Artifact,
    compact: &str,
) -> Result<()> {
    ensure!(compact.len() <= 4096, "JWS превышает 4096 байт");
    let parts: Vec<_> = compact.trim().split('.').collect();
    ensure!(parts.len() == 3, "Ожидается JWS Compact");
    let mut decoded = Vec::new();
    for part in &parts {
        let bytes = URL_SAFE_NO_PAD.decode(part)?;
        ensure!(
            URL_SAFE_NO_PAD.encode(&bytes) == *part,
            "Неканонический JWS base64url"
        );
        decoded.push(bytes);
    }
    let header: Header = serde_json::from_slice(&decoded[0])?;
    let (public, kid) = jwk(project)?;
    ensure!(
        header
            == Header {
                alg: "EdDSA".into(),
                typ: "releases-standalone+jws".into(),
                kid
            },
        "Неверные заголовок или kid JWS"
    );
    let actual: Payload = serde_json::from_slice(&decoded[1])?;
    ensure!(
        actual == payload(project, version, artifact),
        "JWS не соответствует продукту, версии, платформе, размеру или SHA-256"
    );
    let signature = Signature::from_slice(&decoded[2])?;
    public
        .verify_strict(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .context("Неверная подпись Ed25519")?;
    Ok(())
}

fn tauri_public(project: &Project) -> Result<minisign::PublicKey> {
    let value = project.identity["plugins"]["updater"]["pubkey"]
        .as_str()
        .context("Нет закреплённого ключа Tauri")?;
    let bytes = STANDARD.decode(value)?;
    Ok(minisign::PublicKeyBox::from_string(std::str::from_utf8(&bytes)?)?.into_public_key()?)
}

pub fn verify_tauri(project: &Project, file: &Path, signature: &str) -> Result<()> {
    ensure!(signature.len() <= 16384, "Слишком большая подпись Tauri");
    let decoded = STANDARD
        .decode(signature.trim())
        .context("Подпись Tauri должна быть base64 minisign box")?;
    let signature = minisign::SignatureBox::from_string(std::str::from_utf8(&decoded)?)?;
    minisign::verify(
        &tauri_public(project)?,
        &signature,
        fs::File::open(file)?,
        true,
        false,
        false,
    )
    .context("Неверная подпись Tauri updater")?;
    Ok(())
}
