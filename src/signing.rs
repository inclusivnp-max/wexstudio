//! Explicit release signing. Only environment references/public certificate metadata persist.
use crate::{build::Step, run::RunEvent};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub keystore: PathBuf,
    pub key_alias: String,
    pub store_password_env: String,
    pub key_password_env: String,
    pub certificate_sha256: String,
}
pub fn certificate_pin(s: &str) -> Result<String> {
    let s = s.replace(':', "");
    ensure!(
        s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()),
        "certificate_sha256 must contain64 hex digits"
    );
    Ok(s.to_ascii_lowercase())
}
fn env_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.keystore.as_os_str().is_empty()
                && (self.keystore.is_absolute()
                    || self
                        .keystore
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)))),
            "release keystore path must be absolute or project relative without traversal"
        );
        ensure!(
            !self.key_alias.trim().is_empty()
                && self.key_alias.len() <= 128
                && !self.key_alias.starts_with('-')
                && !self.key_alias.chars().any(char::is_control),
            "invalid signing key alias"
        );
        ensure!(
            env_name(&self.store_password_env) && env_name(&self.key_password_env),
            "signing passwords require environment variable names, not values"
        );
        ensure!(
            ![&self.store_password_env, &self.key_password_env]
                .into_iter()
                .any(|name| matches!(
                    name.as_str(),
                    "JAVA_HOME"
                        | "PATH"
                        | "HOME"
                        | "ANDROID_HOME"
                        | "ANDROID_SDK_ROOT"
                        | "WEX_DEBUG_STOREPASS"
                        | "WEX_PRIVATE_STOREPASS"
                        | "WEX_PRIVATE_KEYPASS"
                )),
            "signing secrets require dedicated environment variable names"
        );
        certificate_pin(&self.certificate_sha256)?;
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Info {
    pub kind: String,
    pub keystore: PathBuf,
    pub key_alias: String,
    pub keystore_blake3: String,
    pub certificate_sha256: String,
}
// No Debug/Serialize implementation on secret-bearing state.
pub struct Prepared {
    pub keystore: PathBuf,
    pub alias: String,
    pin: String,
    hash: String,
    store: String,
    key: String,
    env_names: [String; 2],
}
fn password(name: &str) -> Result<String> {
    let value = match std::env::var(name) {
        Ok(s) => s,
        Err(_) => bail!("Signing environment variable {name} missing or not UTF8"),
    };
    ensure!(
        !value.is_empty() && value.len() <= 16384 && !value.contains('\0'),
        "Signing environment variable {name} is empty/invalid"
    );
    Ok(value)
}
fn key_hash(path: &Path) -> Result<String> {
    ensure!(
        path.is_file() && std::fs::metadata(path)?.len() <= 16 * 1024 * 1024,
        "release keystore missing or exceeds16MiB"
    );
    Ok(blake3::hash(&std::fs::read(path)?).to_hex().to_string())
}
pub fn certificate(text: &str) -> Result<String> {
    let lines = text
        .lines()
        .filter_map(|l| {
            l.trim().strip_prefix("SHA256:").or_else(|| {
                l.trim()
                    .strip_prefix("Signer #1 certificate SHA-256 digest:")
            })
        })
        .map(str::trim)
        .collect::<Vec<_>>();
    ensure!(
        lines.len() == 1,
        "signer returned no unique certificate SHA256"
    );
    certificate_pin(lines[0])
}
fn keytool_certificate(text: &str) -> Result<String> {
    // -list -v prints all certificates in a PrivateKeyEntry's chain. Pin the
    // first (leaf) certificate, not a CA/trusted entry or an arbitrary last hash.
    if let Some((_, chain)) = text.split_once("Certificate[1]:") {
        certificate(chain.split("Certificate[2]:").next().unwrap_or(chain))
    } else {
        certificate(text)
    }
}
fn redact(s: &str, store: &str, key: &str) -> String {
    let mut values = [store, key];
    values.sort_by_key(|s| std::cmp::Reverse(s.len()));
    values
        .into_iter()
        .filter(|s| !s.is_empty())
        .fold(s.to_owned(), |s, v| s.replace(v, "[REDACTED]"))
}
impl Prepared {
    pub fn load(root: &Path, cfg: &Config) -> Result<Self> {
        cfg.validate()?;
        let keystore = if cfg.keystore.is_absolute() {
            cfg.keystore.clone()
        } else {
            root.join(&cfg.keystore)
        }
        .canonicalize()
        .context("release keystore missing; configured key is never auto-created")?;
        Ok(Self {
            hash: key_hash(&keystore)?,
            keystore,
            alias: cfg.key_alias.clone(),
            pin: certificate_pin(&cfg.certificate_sha256)?,
            store: password(&cfg.store_password_env)?,
            key: password(&cfg.key_password_env)?,
            env_names: [cfg.store_password_env.clone(), cfg.key_password_env.clone()],
        })
    }
    async fn execute(
        &self,
        jdk: &Path,
        program: &Path,
        args: Vec<OsString>,
        name: &str,
        event: &(dyn Fn(RunEvent) + Send + Sync),
    ) -> Result<(Step, Option<String>)> {
        event(RunEvent::Phase(name.into()));
        let start = Instant::now();
        let mut command = tokio::process::Command::new(program);
        command
            .args(&args)
            .env("JAVA_HOME", jdk)
            .env("WEX_PRIVATE_STOREPASS", &self.store)
            .env("WEX_PRIVATE_KEYPASS", &self.key)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        for key in &self.env_names {
            command.env_remove(key);
        }
        // Internal variable names win even if the user chose those names as references.
        command
            .env("WEX_PRIVATE_STOREPASS", &self.store)
            .env("WEX_PRIVATE_KEYPASS", &self.key);
        let mut child = command.spawn().context("starting signing tool")?;
        use tokio::io::AsyncReadExt;
        async fn read<R: tokio::io::AsyncRead + Unpin>(r: R) -> Result<String> {
            let mut b = vec![];
            r.take(4 * 1024 * 1024 + 1).read_to_end(&mut b).await?;
            ensure!(b.len() <= 4 * 1024 * 1024, "signing output exceeds4MiB");
            Ok(String::from_utf8_lossy(&b).into_owned())
        }
        let out = child.stdout.take().context("signer stdout")?;
        let err = child.stderr.take().context("signer stderr")?;
        let (out, err, status) = tokio::time::timeout(Duration::from_secs(90), async {
            tokio::try_join!(read(out), read(err), async {
                Ok::<_, anyhow::Error>(child.wait().await?)
            })
        })
        .await
        .context("signing tool timed out")??;
        let cert = if name == "Release signing preflight" {
            keytool_certificate(&out)
        } else {
            certificate(&out)
        }
        .ok();
        let stdout = redact(&out, &self.store, &self.key);
        let stderr = redact(&err, &self.store, &self.key);
        if !stdout.is_empty() {
            event(RunEvent::BuildLine(stdout.clone()));
        }
        if !stderr.is_empty() {
            event(RunEvent::BuildLine(stderr.clone()));
        }
        ensure!(
            status.success(),
            "{name} failed ({status}): {stdout}{stderr}"
        );
        Ok((
            Step {
                name: name.into(),
                program: program.into(),
                args: args.iter().map(|s| s.to_string_lossy().into()).collect(),
                exit_code: status.code().unwrap_or(-1),
                stdout,
                stderr,
                elapsed_ms: start.elapsed().as_millis(),
            },
            cert,
        ))
    }
    pub async fn preflight(
        &self,
        jdk: &Path,
        event: &(dyn Fn(RunEvent) + Send + Sync),
    ) -> Result<Step> {
        let (step, cert) = self
            .execute(
                jdk,
                &jdk.join("bin/keytool"),
                [
                    "-J-Duser.language=en",
                    "-J-Duser.country=US",
                    "-list",
                    "-v",
                    "-keystore",
                ]
                .map(OsString::from)
                .into_iter()
                .chain([
                    self.keystore.as_os_str().to_owned(),
                    "-alias".into(),
                    self.alias.clone().into(),
                    "-storepass:env".into(),
                    "WEX_PRIVATE_STOREPASS".into(),
                ])
                .collect(),
                "Release signing preflight",
                event,
            )
            .await?;
        ensure!(
            cert.as_deref() == Some(self.pin.as_str()),
            "release signing certificate does not match configured SHA256 pin"
        );
        Ok(step)
    }
    pub async fn sign(
        &self,
        jdk: &Path,
        apksigner: &Path,
        input: &Path,
        output: &Path,
        event: &(dyn Fn(RunEvent) + Send + Sync),
    ) -> Result<Step> {
        self.unchanged()?;
        Ok(self
            .execute(
                jdk,
                apksigner,
                vec![
                    "sign".into(),
                    "--ks".into(),
                    self.keystore.as_os_str().to_owned(),
                    "--ks-key-alias".into(),
                    self.alias.clone().into(),
                    "--ks-pass".into(),
                    "env:WEX_PRIVATE_STOREPASS".into(),
                    "--key-pass".into(),
                    "env:WEX_PRIVATE_KEYPASS".into(),
                    "--v4-signing-enabled".into(),
                    "false".into(),
                    "--out".into(),
                    output.as_os_str().to_owned(),
                    input.as_os_str().to_owned(),
                ],
                "APK sign · configured release key",
                event,
            )
            .await?
            .0)
    }
    pub async fn verify(
        &self,
        jdk: &Path,
        apksigner: &Path,
        apk: &Path,
        event: &(dyn Fn(RunEvent) + Send + Sync),
    ) -> Result<(Step, Info)> {
        let (step, cert) = self
            .execute(
                jdk,
                apksigner,
                vec![
                    "verify".into(),
                    "--verbose".into(),
                    "--print-certs".into(),
                    apk.as_os_str().to_owned(),
                ],
                "Verify release APK signature/pin",
                event,
            )
            .await?;
        ensure!(
            cert.as_deref() == Some(self.pin.as_str()),
            "signed APK certificate differs from release pin"
        );
        self.unchanged()?;
        Ok((
            step,
            Info {
                kind: "release".into(),
                keystore: self.keystore.clone(),
                key_alias: self.alias.clone(),
                keystore_blake3: self.hash.clone(),
                certificate_sha256: self.pin.clone(),
            },
        ))
    }
    pub fn unchanged(&self) -> Result<()> {
        ensure!(
            key_hash(&self.keystore)? == self.hash,
            "signing keystore changed during build; artifact not published"
        );
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keytool_chain_pin_selects_leaf_not_ca() {
        let leaf = "ab".repeat(32);
        let ca = "cd".repeat(32);
        let text = format!(
            "Certificate chain length: 2\nCertificate[1]:\nSHA256: {leaf}\nCertificate[2]:\nSHA256: {ca}"
        );
        assert_eq!(keytool_certificate(&text).unwrap(), leaf);
    }
    #[test]
    fn certificate_pins_are_exact_and_normalized() {
        let h = "AB".repeat(32);
        assert_eq!(certificate_pin(&h).unwrap(), "ab".repeat(32));
        assert!(certificate_pin("abc").is_err());
        assert!(certificate_pin(&"gg".repeat(32)).is_err());
    }
    #[test]
    fn environment_names_reject_values_flags_and_controls() {
        for s in ["", "1PASS", "a=b", "env:PASS", "secret value", "PASS\n"] {
            assert!(!env_name(s));
        }
        assert!(env_name("WEX_RELEASE_PASS_2"));
    }
    #[test]
    fn redaction_removes_longest_secret_before_prefix() {
        assert_eq!(
            redact("topsecret123 topsecret", "topsecret", "topsecret123"),
            "[REDACTED] [REDACTED]"
        );
    }
    #[test]
    fn certificate_parser_rejects_missing_and_ambiguous_output() {
        let h = "ab".repeat(32);
        assert_eq!(certificate(&format!("SHA256: {h}")).unwrap(), h);
        assert!(certificate("SHA1: ab").is_err());
        assert!(certificate(&format!("SHA256: {h}\nSHA256: {h}")).is_err());
    }
    #[test]
    fn signing_schema_never_accepts_raw_password_fields() {
        let s = r#"{"keystore":"release.jks","key_alias":"release","store_password_env":"STORE","key_password_env":"KEY","certificate_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","password":"raw"}"#;
        assert!(serde_json::from_str::<Config>(s).is_err());
    }
}
