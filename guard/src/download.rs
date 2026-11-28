//! Gets a model's files at a pinned revision and checks their SHA-256.

use crate::config::{Config, Source};
use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// The local paths of a model's graph and tokenizer.
#[derive(Clone, Debug)]
pub struct Files {
    pub model: PathBuf,
    pub tokenizer: PathBuf,
}

/// Where the files of `source` are kept in `dir`.
pub fn files(dir: &Path, source: &Source) -> Files {
    Files {
        model: dir.join(&source.model_file),
        tokenizer: dir.join(&source.tokenizer_file),
    }
}

/// Download each missing file of the configured model, and return where the
/// files are. A file is written as `NAME.part` and renamed once its SHA-256
/// matches, so a file in the cache is always complete and checked.
pub fn ensure(config: &Config) -> Result<Files> {
    let source = config.source()?;
    let dir = config.model_dir()?;
    let local = files(&dir, &source);
    let token = std::env::var(&config.token_env)
        .ok()
        .filter(|t| !t.trim().is_empty());
    let wanted = [
        (&source.model_file, &source.model_sha256, &local.model),
        (
            &source.tokenizer_file,
            &source.tokenizer_sha256,
            &local.tokenizer,
        ),
    ];
    for (name, sha256, target) in wanted {
        if target.exists() {
            continue;
        }
        let url = format!(
            "{}/{}/resolve/{}/{name}",
            config.endpoint.trim_end_matches('/'),
            source.repository,
            source.revision
        );
        fetch(&url, token.as_deref(), sha256, target)?;
    }
    Ok(local)
}

fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_connect(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// Write `url` to `target` when its SHA-256 is `sha256`.
fn fetch(url: &str, token: Option<&str>, sha256: &str, target: &Path) -> Result<()> {
    eprintln!("amaru-guard: downloading {url}");
    let mut request = agent().get(url);
    if let Some(token) = token {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    let mut response = request
        .call()
        .with_context(|| format!("cannot get {url}"))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let mut partial = target.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let checked = copy_hashed(response.body_mut().as_reader(), &partial)
        .with_context(|| format!("cannot download {url}"))
        .and_then(|digest| {
            if digest.eq_ignore_ascii_case(sha256) {
                Ok(())
            } else {
                Err(anyhow!("{url} has SHA-256 {digest}, expected {sha256}"))
            }
        });
    if let Err(e) = checked {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    std::fs::rename(&partial, target).with_context(|| format!("cannot write {}", target.display()))
}

/// Copy `reader` to a new file at `path`, returning the SHA-256 in hex.
fn copy_hashed(mut reader: impl Read, path: &Path) -> Result<String> {
    let file = File::create(path).with_context(|| format!("cannot write {}", path.display()))?;
    let mut out = BufWriter::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        out.write_all(&buffer[..read])?;
    }
    out.flush()?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_while_it_copies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let digest = copy_hashed(&b"abc"[..], &path).unwrap();
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
    }

    #[test]
    fn uses_files_already_in_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = serde_yaml::from_str(&format!(
            "cache_dir: {}\nendpoint: http://127.0.0.1:9\n",
            dir.path().display()
        ))
        .unwrap();
        let source = config.source().unwrap();
        let local = files(&config.model_dir().unwrap(), &source);
        assert!(ensure(&config).is_err(), "nothing is cached yet");
        for path in [&local.model, &local.tokenizer] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        let found = ensure(&config).unwrap();
        assert_eq!(found.model, local.model);
    }
}
