use crate::{
    artifacts::{self, Spec},
    config::Project,
    git,
    plan::Plan,
    signing::{self, Material},
};
use anyhow::Result;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{SigningKey, pkcs8::EncodePrivateKey};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub root: PathBuf,
}

impl Fixture {
    pub fn new(seed: &str, push: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repository");
        fs::create_dir(&root).unwrap();
        git::read(&root, &["init", "-b", "main"]).unwrap();
        git::read(&root, &["config", "user.name", "Release Test"]).unwrap();
        git::read(&root, &["config", "user.email", "release@example.invalid"]).unwrap();
        git::read(&root, &["config", "commit.gpgsign", "false"]).unwrap();
        let key = SigningKey::from_bytes(&[7; 32]);
        let key_path = temp.path().join("test-key.pem");
        fs::write(
            &key_path,
            key.to_pkcs8_pem(pkcs8::LineEnding::LF).unwrap().as_bytes(),
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname='sample'\nversion='{seed}'\nedition='2024'\n"),
        )
        .unwrap();
        artifacts::write_json(&root.join("identity.json"), &json!({"product_id":"b54ca4c7-e588-4844-b7bb-6ae05e59634d", "updates":{"public_key":{"kty":"OKP","crv":"Ed25519","x":URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes())}}})).unwrap();
        let config = format!(
            r#"slug = "sample"
profile = "standalone"
exclude = ["vpn-manager"]
push_tag = {push}
targets = [{{os="linux",arch="x86_64"}}, {{os="windows",arch="x86_64"}}, {{os="macos",arch="aarch64"}}]
[rust]
binary = "sample"
version_env = "SAMPLE_RELEASE_VERSION"
[signing]
identity = "identity.json"
private_key_env = "VERUNA_PACK_TEST_UNUSED_PRIVATE_KEY"
private_key_file = '{}'
"#,
            key_path.display()
        );
        fs::write(root.join("release.toml"), config).unwrap();
        let this = Self { temp, root };
        this.commit("chore: initial");
        this
    }

    pub fn project(&self) -> Project {
        Project::load(&self.root.join("release.toml")).unwrap()
    }
    pub fn use_defaults(&self) {
        fs::remove_file(self.root.join("release.toml")).unwrap();
        fs::rename(
            self.root.join("identity.json"),
            self.root.join("release.config.json"),
        )
        .unwrap();
        fs::create_dir_all(self.root.join("src")).unwrap();
        fs::write(self.root.join("src/main.rs"), "fn main() {}\n").unwrap();
    }
    pub fn git(&self, args: &[&str]) -> String {
        git::read(&self.root, args).unwrap()
    }
    pub fn commit(&self, message: &str) {
        let marker = self.root.join("source.txt");
        let value = fs::read_to_string(&marker).unwrap_or_default() + message + "\n";
        fs::write(marker, value).unwrap();
        self.git(&["add", "--all", "--", ".", ":(exclude,literal)releases"]);
        self.git(&["commit", "-m", message]);
    }
    pub fn base(&self, tag: &str) {
        self.git(&["tag", tag]);
    }
    pub fn release(&self) -> PathBuf {
        self.root.join("releases/0.3.0")
    }
    pub fn seeded(push: bool) -> Self {
        let f = Self::new("0.2.4", push);
        f.base("v0.2.4");
        f.commit("feat(search): поиск по ИНН");
        f.commit("fix(updater): повтор загрузки после обрыва");
        f.commit("chore(ci): обновить кеш");
        f
    }
    pub fn bare_origin(&self) -> PathBuf {
        let origin = self.temp.path().join("origin.git");
        assert!(
            Command::new("git")
                .args(["init", "--bare"])
                .arg(&origin)
                .output()
                .unwrap()
                .status
                .success()
        );
        self.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
        origin
    }
}

// Transaction tests use deterministic bytes; OS package inspection is tested separately
// and the product rehearsals exercise the real compilers, packagers and clients.
pub fn fake_build(
    project: &Project,
    plan: &Plan,
    output: &Path,
    _: &Path,
    _: &Material,
) -> Result<()> {
    for spec in artifacts::specs(project, plan)
        .iter()
        .filter(|s| s.roles != ["signature"])
    {
        fs::write(
            output.join(&spec.name),
            format!("fixture {} {}", plan.version.as_ref().unwrap(), spec.name),
        )?;
    }
    Ok(())
}
pub fn verify_transaction(project: &Project, plan: &Plan, path: &Path) -> Result<()> {
    artifacts::verify_using(
        project,
        plan,
        path,
        |_: &Project, _: &Plan, _: &Path, _: &Spec| Ok(()),
    )
}

#[test]
fn jws_rejects_wrong_key_even_with_trusted_kid_and_correct_hashes() {
    let f = Fixture::seeded(false);
    let p = f.project();
    let plan = crate::plan::calculate(&p).unwrap();
    let output = f.temp.path().join("bundle");
    fs::create_dir(&output).unwrap();
    let wrong = Material::Standalone(Box::new(SigningKey::from_bytes(&[9; 32])));
    fake_build(&p, &plan, &output, &output, &wrong).unwrap();
    artifacts::finish(&p, &plan, &output, &wrong).unwrap();
    assert!(
        format!("{:#}", verify_transaction(&p, &plan, &output).unwrap_err()).contains("подпись")
    );
}

#[test]
fn jws_binds_every_field_and_rejects_duplicate_json_keys() {
    let f = Fixture::seeded(false);
    let p = f.project();
    let plan = crate::plan::calculate(&p).unwrap();
    let output = f.temp.path().join("bundle");
    fs::create_dir(&output).unwrap();
    let material = Material::load(&p).unwrap();
    fake_build(&p, &plan, &output, &output, &material).unwrap();
    artifacts::finish(&p, &plan, &output, &material).unwrap();
    let manifest: artifacts::Manifest =
        serde_json::from_slice(&fs::read(output.join("release.json")).unwrap()).unwrap();
    let artifact = manifest.artifacts[0].clone();
    let compact = fs::read_to_string(output.join(artifact.signature.as_ref().unwrap())).unwrap();
    signing::verify_jws(&p, "0.3.0", &artifact, &compact).unwrap();
    for field in 0..5 {
        let mut a = artifact.clone();
        match field {
            0 => a.spec.arch = "aarch64".into(),
            1 => a.spec.protocol_os = "darwin".into(),
            2 => a.size_bytes += 1,
            3 => a.sha256 = "0".repeat(64),
            _ => (),
        };
        assert!(
            signing::verify_jws(&p, if field == 4 { "0.3.1" } else { "0.3.0" }, &a, &compact)
                .is_err()
        );
    }
    let mut parts: Vec<_> = compact.trim().split('.').map(str::to_owned).collect();
    let header = String::from_utf8(URL_SAFE_NO_PAD.decode(&parts[0]).unwrap()).unwrap();
    parts[0] = URL_SAFE_NO_PAD.encode(header.replacen('{', "{\"alg\":\"EdDSA\",", 1));
    assert!(signing::verify_jws(&p, "0.3.0", &artifact, &parts.join(".")).is_err());
}

#[test]
fn tauri_signatures_verify_actual_bytes_not_only_key_id() {
    use base64::engine::general_purpose::STANDARD;
    use std::io::Cursor;
    let f = Fixture::seeded(false);
    let mut p = f.project();
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let public = STANDARD.encode(pair.pk.to_box().unwrap().to_string());
    p.identity =
        json!({"identifier":"com.hostutil.desktop", "plugins":{"updater":{"pubkey":public}}});
    let path = f.temp.path().join("update.app.tar.gz");
    fs::write(&path, b"actual update package").unwrap();
    let signature = minisign::sign(
        Some(&pair.pk),
        &pair.sk,
        Cursor::new(b"actual update package"),
        Some("test update"),
        None,
    )
    .unwrap();
    let signature = STANDARD.encode(signature.into_string());
    signing::verify_tauri(&p, &path, &signature).unwrap();
    fs::write(&path, b"modified update package").unwrap();
    assert!(signing::verify_tauri(&p, &path, &signature).is_err());
}
