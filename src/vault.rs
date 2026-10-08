use std::collections::BTreeMap;
use std::env;
#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

const MAGIC: &[u8; 5] = b"MOLSO";
const VERSION: u8 = 1;
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const HEADER_LEN: usize = MAGIC.len() + 1;

#[derive(Debug)]
pub struct Paths {
    pub vault: PathBuf,
    pub key: PathBuf,
    pub lock: PathBuf,
}

impl Paths {
    /// Resolve vault and key paths with precedence: CLI argument > environment
    /// variable (path only) > platform local data directory. Paths are made
    /// absolute against the current working directory; relative inputs stay
    /// relative to the invoking cwd.
    pub fn resolve(vault: Option<PathBuf>, key: Option<PathBuf>) -> Result<Paths> {
        let vault = match vault.or_else(|| env::var_os("MOLSO_VAULT").map(PathBuf::from)) {
            Some(path) => absolute(&path)?,
            None => default_dir()?.join("vault"),
        };
        let key = match key.or_else(|| env::var_os("MOLSO_KEY_FILE").map(PathBuf::from)) {
            Some(path) => absolute(&path)?,
            None => default_dir()?.join("key"),
        };
        let lock = lock_path(&vault);
        Ok(Paths { vault, key, lock })
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path)
        .map_err(|e| Error::usage(format!("invalid path {}: {e}", path.display())))
}

fn default_dir() -> Result<PathBuf> {
    dirs::data_local_dir()
        .map(|dir| dir.join("molso"))
        .ok_or_else(|| Error::failure("cannot determine the platform local data directory"))
}

fn lock_path(vault: &Path) -> PathBuf {
    let mut name = vault.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

fn ensure_parent(path: &Path, create: bool) -> Result<()> {
    if let Some(parent) = path.parent()
        && create
    {
        create_dir_all_mode(parent, 0o700)?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_dir_all_mode(dir: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(mode);
    builder.create(dir)
}

#[cfg(windows)]
fn create_dir_all_mode(dir: &Path, _mode: u32) -> io::Result<()> {
    crate::windows::create_dir_all(dir)
}

#[cfg(not(any(unix, windows)))]
fn create_dir_all_mode(dir: &Path, _mode: u32) -> io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn open_create_new(path: &Path, mode: u32) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
}

#[cfg(windows)]
fn open_create_new(path: &Path, _mode: u32) -> io::Result<File> {
    crate::windows::create_new(path)
}

#[cfg(not(any(unix, windows)))]
fn open_create_new(path: &Path, _mode: u32) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn open_lock(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

#[cfg(windows)]
fn open_lock(path: &Path) -> io::Result<File> {
    crate::windows::open_lock(path)
}

#[cfg(not(any(unix, windows)))]
fn open_lock(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    // Directory fsync is not available through std on Windows; durability
    // semantics there need native verification and are not claimed here.
    Ok(())
}

/// A stable advisory lock on `<vault>.lock`. The lock file is never replaced,
/// so it survives `rename`-based vault replacement. Dropping releases the lock.
pub struct VaultLock {
    _file: File,
}

impl VaultLock {
    pub fn exclusive(path: &Path) -> Result<VaultLock> {
        let file = open_lock(path)?;
        file.lock()
            .map_err(|e| Error::failure(format!("cannot lock vault: {e}")))?;
        Ok(VaultLock { _file: file })
    }

    pub fn shared(path: &Path) -> Result<VaultLock> {
        let file = open_lock(path)?;
        file.lock_shared()
            .map_err(|e| Error::failure(format!("cannot lock vault: {e}")))?;
        Ok(VaultLock { _file: file })
    }
}

#[derive(Serialize, Deserialize, Default)]
struct VaultData {
    entries: BTreeMap<String, Zeroizing<Vec<u8>>>,
}

fn header() -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..MAGIC.len()].copy_from_slice(MAGIC);
    header[MAGIC.len()] = VERSION;
    header
}

fn encrypt(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher =
        XChaCha20Poly1305::new_from_slice(key).map_err(|_| Error::failure("invalid key length"))?;
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let header = header();
    let ciphertext = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plaintext,
                aad: &header,
            },
        )
        .map_err(|_| Error::failure("failed to encrypt vault"))?;
    let mut out = Vec::with_capacity(HEADER_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn decrypt(key: &[u8], data: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if data.len() < HEADER_LEN + NONCE_LEN {
        return Err(Error::failure(
            "vault file is truncated or not a molso vault",
        ));
    }
    if &data[..MAGIC.len()] != MAGIC {
        return Err(Error::failure("unknown vault format"));
    }
    if data[MAGIC.len()] != VERSION {
        return Err(Error::failure(format!(
            "unsupported vault version {}",
            data[MAGIC.len()]
        )));
    }
    let header = &data[..HEADER_LEN];
    let nonce = &data[HEADER_LEN..HEADER_LEN + NONCE_LEN];
    let ciphertext = &data[HEADER_LEN + NONCE_LEN..];
    let cipher =
        XChaCha20Poly1305::new_from_slice(key).map_err(|_| Error::failure("invalid key length"))?;
    let nonce = XNonce::try_from(nonce).map_err(|_| Error::failure("invalid nonce"))?;
    let plaintext = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: header,
            },
        )
        .map_err(|_| {
            Error::failure("vault authentication failed (wrong key or corrupted vault)")
        })?;
    Ok(Zeroizing::new(plaintext))
}

fn encode(key: &[u8], data: &VaultData) -> Result<Vec<u8>> {
    let plaintext = Zeroizing::new(
        serde_json::to_vec(data).map_err(|_| Error::failure("failed to serialize vault"))?,
    );
    encrypt(key, plaintext.as_slice())
}

fn decode(key: &[u8], raw: &[u8]) -> Result<VaultData> {
    let plaintext = decrypt(key, raw)?;
    serde_json::from_slice(plaintext.as_slice())
        .map_err(|_| Error::failure("vault content is not valid"))
}

fn read_key(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let bytes =
        Zeroizing::new(fs::read(path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                Error::failure(format!(
                    "key file is missing: {}. For a new store, run molso init; for an existing store, restore the original key or check --key-file",
                    path.display()
                ))
            } else {
                Error::failure(format!("cannot read key file {}: {e}", path.display()))
            }
        })?);
    if bytes.len() != KEY_LEN {
        return Err(Error::failure(format!(
            "key file {} must be exactly {KEY_LEN} bytes (found {})",
            path.display(),
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn read_vault_file(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let bytes = fs::read(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            Error::failure(format!(
                "vault is missing: {}. Check --vault or restore the vault; molso init creates an empty vault using the existing key",
                path.display()
            ))
        } else {
            Error::failure(format!("cannot read vault {}: {e}", path.display()))
        }
    })?;
    Ok(Zeroizing::new(bytes))
}

fn write_temp(target: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("vault");
    for _ in 0..64 {
        let mut suffix = [0u8; 8];
        rand::rngs::OsRng.fill_bytes(&mut suffix);
        let candidate = parent.join(format!(
            ".{file_name}.tmp.{}.{:016x}",
            std::process::id(),
            u64::from_le_bytes(suffix)
        ));
        match open_create_new(&candidate, 0o600) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(bytes).and_then(|_| file.sync_all()) {
                    let _ = fs::remove_file(&candidate);
                    return Err(e.into());
                }
                return Ok(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::failure("could not create a temporary vault file"))
}

/// Publish a brand-new file, failing if the target already exists. Uses a
/// hard-link commit so a concurrent creation can never be overwritten.
fn publish_new(target: &Path, bytes: &[u8]) -> Result<()> {
    let temp = write_temp(target, bytes)?;
    let link_result = fs::hard_link(&temp, target);
    let _ = fs::remove_file(&temp);
    match link_result {
        Ok(()) => match sync_dir(target.parent().unwrap_or_else(|| Path::new("."))) {
            Ok(()) => Ok(()),
            Err(e) => Err(Error::failure(format!(
                "file committed but directory sync failed; durability is not confirmed: {e}"
            ))),
        },
        Err(e) => {
            if target.symlink_metadata().is_ok() {
                Err(Error::failure(format!(
                    "refusing to overwrite existing {}",
                    target.display()
                )))
            } else {
                Err(e.into())
            }
        }
    }
}

/// Replace an existing file through a same-directory temporary file. `rename`
/// is the commit point. A failure to sync the parent directory afterwards means
/// the new content is committed but durability is unconfirmed.
fn replace(target: &Path, bytes: &[u8]) -> Result<()> {
    let temp = write_temp(target, bytes)?;
    if let Err(e) = fs::rename(&temp, target) {
        let _ = fs::remove_file(&temp);
        return Err(e.into());
    }
    match sync_dir(target.parent().unwrap_or_else(|| Path::new("."))) {
        Ok(()) => Ok(()),
        Err(e) => Err(Error::failure(format!(
            "vault committed but directory sync failed; durability is not confirmed: {e}"
        ))),
    }
}

pub fn init(paths: &Paths) -> Result<()> {
    ensure_parent(&paths.vault, true)?;
    ensure_parent(&paths.key, true)?;
    let _lock = VaultLock::exclusive(&paths.lock)?;

    let key_exists = paths.key.symlink_metadata().is_ok();
    let vault_exists = paths.vault.symlink_metadata().is_ok();
    match (key_exists, vault_exists) {
        (false, false) => {
            let mut key = Zeroizing::new(vec![0u8; KEY_LEN]);
            rand::rngs::OsRng.fill_bytes(key.as_mut_slice());
            publish_new(&paths.key, key.as_slice())?;
            let empty = encode(key.as_slice(), &VaultData::default())?;
            publish_new(&paths.vault, &empty)?;
        }
        (true, false) => {
            let key = read_key(&paths.key)?;
            let empty = encode(key.as_slice(), &VaultData::default())?;
            publish_new(&paths.vault, &empty)?;
        }
        (false, true) => {
            return Err(Error::failure(
                "vault exists but key file is missing; refusing to generate a new key because the vault would become undecryptable",
            ));
        }
        (true, true) => {
            return Err(Error::failure(
                "already initialized: both key and vault exist; run molso list to see credentials or molso put SERVICE/ACCOUNT to add one",
            ));
        }
    }
    Ok(())
}

fn validate_put(data: &VaultData, name: &str, update: bool) -> Result<()> {
    let exists = data.entries.contains_key(name);
    if exists && !update {
        return Err(Error::failure(format!(
            "{name} already exists; pass --update to replace it"
        )));
    }
    if !exists && update {
        return Err(Error::failure(format!(
            "{name} does not exist; omit --update to create it, or run molso list to see stored names"
        )));
    }
    Ok(())
}

pub fn check_put(paths: &Paths, name: &str, update: bool) -> Result<()> {
    let key = read_key(&paths.key)?;
    let _lock = VaultLock::shared(&paths.lock)?;
    let raw = read_vault_file(&paths.vault)?;
    let data = decode(key.as_slice(), raw.as_slice())?;
    validate_put(&data, name, update)
}

pub fn put(paths: &Paths, name: &str, update: bool, secret: Zeroizing<Vec<u8>>) -> Result<()> {
    let key = read_key(&paths.key)?;
    let _lock = VaultLock::exclusive(&paths.lock)?;
    let raw = read_vault_file(&paths.vault)?;
    let mut data = decode(key.as_slice(), raw.as_slice())?;

    // Recheck under the write lock: another process may update while input is read.
    validate_put(&data, name, update)?;
    data.entries.insert(name.to_string(), secret);

    let encoded = encode(key.as_slice(), &data)?;
    replace(&paths.vault, &encoded)
}

pub fn list(paths: &Paths) -> Result<Vec<String>> {
    let key = read_key(&paths.key)?;
    let _lock = VaultLock::shared(&paths.lock)?;
    let raw = read_vault_file(&paths.vault)?;
    let data = decode(key.as_slice(), raw.as_slice())?;
    Ok(data.entries.keys().cloned().collect())
}

pub fn get(paths: &Paths, name: &str) -> Result<Zeroizing<Vec<u8>>> {
    let key = read_key(&paths.key)?;
    let value = {
        let _lock = VaultLock::shared(&paths.lock)?;
        let raw = read_vault_file(&paths.vault)?;
        let data = decode(key.as_slice(), raw.as_slice())?;
        data.entries.get(name).cloned().ok_or_else(|| {
            Error::failure(format!(
                "{name} not found; run molso list to see stored names"
            ))
        })?
    };
    Ok(value)
}

pub fn get_many(paths: &Paths, names: &[&str]) -> Result<Vec<Zeroizing<Vec<u8>>>> {
    let key = read_key(&paths.key)?;
    let values = {
        let _lock = VaultLock::shared(&paths.lock)?;
        let raw = read_vault_file(&paths.vault)?;
        let data = decode(key.as_slice(), raw.as_slice())?;
        let mut values = Vec::with_capacity(names.len());
        for name in names {
            values.push(data.entries.get(*name).cloned().ok_or_else(|| {
                Error::failure(format!(
                    "{name} not found; run molso list to see stored names"
                ))
            })?);
        }
        values
    };
    Ok(values)
}

pub fn remove(paths: &Paths, name: &str) -> Result<()> {
    let key = read_key(&paths.key)?;
    let _lock = VaultLock::exclusive(&paths.lock)?;
    let raw = read_vault_file(&paths.vault)?;
    let mut data = decode(key.as_slice(), raw.as_slice())?;
    if data.entries.remove(name).is_none() {
        return Err(Error::failure(format!(
            "{name} not found; run molso list to see stored names"
        )));
    }
    let encoded = encode(key.as_slice(), &data)?;
    replace(&paths.vault, &encoded)
}
