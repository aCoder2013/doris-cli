use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::{config_home, Config};

const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClusterStore {
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub clusters: BTreeMap<String, Config>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EncryptedStore {
    version: u8,
    nonce: String,
    ciphertext: String,
}

impl ClusterStore {
    pub fn load() -> Result<Self> {
        let path = store_path().context("could not determine cluster store path")?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let encrypted: EncryptedStore = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse encrypted store {}", path.display()))?;
        anyhow::ensure!(encrypted.version == 1, "unsupported cluster store version");

        let key = load_or_create_key()?;
        let nonce_bytes = STANDARD
            .decode(encrypted.nonce.as_bytes())
            .context("failed to decode cluster store nonce")?;
        anyhow::ensure!(
            nonce_bytes.len() == NONCE_BYTES,
            "invalid cluster store nonce"
        );
        let ciphertext = STANDARD
            .decode(encrypted.ciphertext.as_bytes())
            .context("failed to decode cluster store ciphertext")?;

        let cipher = Aes256Gcm::new_from_slice(&key).context("failed to initialize cipher")?;
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
            .map_err(|_| anyhow::anyhow!("failed to decrypt cluster store"))?;
        let store = serde_yaml::from_slice(&plaintext).context("failed to parse cluster store")?;
        Ok(store)
    }

    pub fn save(&self) -> Result<()> {
        let path = store_path().context("could not determine cluster store path")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let key = load_or_create_key()?;
        let cipher = Aes256Gcm::new_from_slice(&key).context("failed to initialize cipher")?;
        let mut nonce_bytes = [0_u8; NONCE_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let plaintext = serde_yaml::to_string(self)?.into_bytes();
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_ref())
            .map_err(|_| anyhow::anyhow!("failed to encrypt cluster store"))?;
        let encrypted = EncryptedStore {
            version: 1,
            nonce: STANDARD.encode(nonce_bytes),
            ciphertext: STANDARD.encode(ciphertext),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&encrypted)?)
            .with_context(|| format!("failed to write {}", path.display()))?;
        secure_file(&path)?;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Config> {
        self.clusters.get(name).cloned()
    }

    pub fn active_config(&self) -> Option<Config> {
        self.active.as_deref().and_then(|name| self.get(name))
    }

    pub fn names(&self) -> Vec<String> {
        self.clusters.keys().cloned().collect()
    }
}

pub fn load_cluster(name: &str) -> Result<Config> {
    let store = ClusterStore::load()?;
    store
        .get(name)
        .with_context(|| format!("saved cluster '{name}' does not exist"))
}

pub fn active_cluster() -> Result<Option<Config>> {
    Ok(ClusterStore::load()?.active_config())
}

pub fn store_path() -> Option<PathBuf> {
    config_home().map(|h| h.join("clusters.enc"))
}

fn key_path() -> Option<PathBuf> {
    config_home().map(|h| h.join("key"))
}

fn load_or_create_key() -> Result<[u8; KEY_BYTES]> {
    let path = key_path().context("could not determine cluster store key path")?;
    if path.exists() {
        let encoded = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let bytes = STANDARD
            .decode(encoded.trim().as_bytes())
            .with_context(|| format!("failed to decode {}", path.display()))?;
        anyhow::ensure!(bytes.len() == KEY_BYTES, "invalid cluster store key length");
        let mut key = [0_u8; KEY_BYTES];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut key = [0_u8; KEY_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut key);
    std::fs::write(&path, STANDARD.encode(key))
        .with_context(|| format!("failed to write {}", path.display()))?;
    secure_file(&path)?;
    Ok(key)
}

fn secure_file(path: &PathBuf) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}
