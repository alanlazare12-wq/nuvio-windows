use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use argon2::Argon2;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const LEGACY_MAGIC: &[u8; 8] = b"NUVIO01\0";
const MAGIC: &[u8; 8] = b"NUVIO02\0";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
pub const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid Nuvio encrypted file")]
    InvalidHeader,
    #[error("invalid encrypted chunk")]
    InvalidChunk,
    #[error("unable to derive encryption key")]
    KeyDerivation,
    #[error("authentication failed while decrypting file")]
    Authentication,
}

pub fn sha256_file(path: &Path) -> Result<String, CryptoError> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

pub fn encrypt_file(input: &Path, output: &Path, passphrase: &str) -> Result<(), CryptoError> {
    if passphrase.is_empty() {
        return Err(CryptoError::KeyDerivation);
    }

    let salt: [u8; SALT_LEN] = rand::random();
    let nonce_prefix: [u8; NONCE_LEN] = rand::random();
    let key = Zeroizing::new(derive_key(passphrase, &salt)?);
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| CryptoError::KeyDerivation)?;

    let mut reader = BufReader::new(File::open(input)?);
    let mut temporary = temporary_output(output)?;
    let mut writer = BufWriter::new(temporary.as_file_mut());

    writer.write_all(MAGIC)?;
    writer.write_all(&salt)?;
    writer.write_all(&nonce_prefix)?;
    writer.write_all(&(DEFAULT_CHUNK_SIZE as u32).to_le_bytes())?;

    let mut buffer = Zeroizing::new(vec![0_u8; DEFAULT_CHUNK_SIZE]);
    let mut chunk_index = 0_u64;

    loop {
        let read = reader.read(&mut buffer)?;
        let nonce_bytes = chunk_nonce(nonce_prefix, chunk_index);
        let aad = chunk_aad(chunk_index, true);
        let encrypted = cipher
            .encrypt(
                &XNonce::from(nonce_bytes),
                Payload {
                    msg: &buffer[..read],
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::Authentication)?;

        writer.write_all(&(read as u32).to_le_bytes())?;
        writer.write_all(&encrypted)?;
        // Authenticate the end of the stream, including for empty files. Without
        // this record, removing complete trailing chunks is indistinguishable from EOF.
        if read == 0 {
            break;
        }
        chunk_index = chunk_index
            .checked_add(1)
            .ok_or(CryptoError::InvalidChunk)?;
    }

    writer.flush()?;
    drop(writer);
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(output)
        .map_err(|error| CryptoError::Io(error.error))?;
    Ok(())
}

pub fn decrypt_file(input: &Path, output: &Path, passphrase: &str) -> Result<(), CryptoError> {
    if passphrase.is_empty() {
        return Err(CryptoError::KeyDerivation);
    }

    let mut reader = BufReader::new(File::open(input)?);
    let mut magic = [0_u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC && &magic != LEGACY_MAGIC {
        return Err(CryptoError::InvalidHeader);
    }

    let mut salt = [0_u8; SALT_LEN];
    let mut nonce_prefix = [0_u8; NONCE_LEN];
    let mut chunk_size_bytes = [0_u8; 4];
    reader.read_exact(&mut salt)?;
    reader.read_exact(&mut nonce_prefix)?;
    reader.read_exact(&mut chunk_size_bytes)?;

    let chunk_size = u32::from_le_bytes(chunk_size_bytes) as usize;
    if chunk_size == 0 || chunk_size > 16 * 1024 * 1024 {
        return Err(CryptoError::InvalidHeader);
    }

    let authenticated_end = &magic == MAGIC;
    let key = Zeroizing::new(derive_key(passphrase, &salt)?);
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| CryptoError::KeyDerivation)?;
    let mut temporary = temporary_output(output)?;
    let mut writer = BufWriter::new(temporary.as_file_mut());
    let mut chunk_index = 0_u64;

    loop {
        let mut plain_len_bytes = [0_u8; 4];
        if reader.read(&mut plain_len_bytes[..1])? == 0 {
            if authenticated_end || chunk_index == 0 {
                return Err(CryptoError::InvalidChunk);
            }
            break;
        }
        reader.read_exact(&mut plain_len_bytes[1..])?;

        let plain_len = u32::from_le_bytes(plain_len_bytes) as usize;
        if (plain_len == 0 && !authenticated_end) || plain_len > chunk_size {
            return Err(CryptoError::InvalidChunk);
        }

        let mut encrypted = vec![0_u8; plain_len + TAG_LEN];
        reader.read_exact(&mut encrypted)?;

        let nonce_bytes = chunk_nonce(nonce_prefix, chunk_index);
        let aad = chunk_aad(chunk_index, authenticated_end);
        let plain = Zeroizing::new(
            cipher
                .decrypt(
                    &XNonce::from(nonce_bytes),
                    Payload {
                        msg: &encrypted,
                        aad: &aad,
                    },
                )
                .map_err(|_| CryptoError::Authentication)?,
        );

        if plain.len() != plain_len {
            return Err(CryptoError::InvalidChunk);
        }
        if plain_len == 0 {
            if reader.read(&mut [0_u8; 1])? != 0 {
                return Err(CryptoError::InvalidChunk);
            }
            break;
        }
        writer.write_all(&plain)?;
        chunk_index = chunk_index
            .checked_add(1)
            .ok_or(CryptoError::InvalidChunk)?;
    }

    writer.flush()?;
    drop(writer);
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(output)
        .map_err(|error| CryptoError::Io(error.error))?;
    Ok(())
}

fn temporary_output(output: &Path) -> Result<tempfile::NamedTempFile, CryptoError> {
    if output.symlink_metadata().is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "El archivo de destino ya existe",
        )
        .into());
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(tempfile::NamedTempFile::new_in(parent)?)
}

fn chunk_aad(index: u64, authenticated_end: bool) -> Vec<u8> {
    let mut aad = Vec::with_capacity(16);
    if authenticated_end {
        aad.extend_from_slice(MAGIC);
    }
    aad.extend_from_slice(&index.to_le_bytes());
    aad
}

fn derive_key(passphrase: &str, salt: &[u8; SALT_LEN]) -> Result<[u8; 32], CryptoError> {
    let mut output = [0_u8; 32];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut output)
        .map_err(|_| CryptoError::KeyDerivation)?;
    Ok(output)
}

fn chunk_nonce(mut base: [u8; NONCE_LEN], chunk_index: u64) -> [u8; NONCE_LEN] {
    base[NONCE_LEN - 8..].copy_from_slice(&chunk_index.to_le_bytes());
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn wrong_password_and_truncation_never_publish_partial_plaintext() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let encrypted = root.path().join("encrypted");
        let restored = root.path().join("restored");
        fs::write(&source, b"private content").unwrap();
        encrypt_file(&source, &encrypted, "correct").unwrap();
        assert!(decrypt_file(&encrypted, &restored, "wrong").is_err());
        assert!(!restored.exists());
        let valid = fs::read(&encrypted).unwrap();
        for length in [53, 55, valid.len() - 1, valid.len() - 20] {
            fs::write(&encrypted, &valid[..length]).unwrap();
            assert!(
                decrypt_file(&encrypted, &restored, "correct").is_err(),
                "accepted truncation at {length}"
            );
            assert!(!restored.exists());
        }
        let mut downgraded = valid[..valid.len() - 20].to_vec();
        downgraded[..8].copy_from_slice(LEGACY_MAGIC);
        fs::write(&encrypted, downgraded).unwrap();
        assert!(decrypt_file(&encrypted, &restored, "correct").is_err());
        assert!(!restored.exists());
        // A header-only legacy file has no authentication at all. Reject it so
        // stripping all modern records and changing the magic cannot bypass verification.
        let mut header_only = valid[..52].to_vec();
        header_only[..8].copy_from_slice(LEGACY_MAGIC);
        fs::write(&encrypted, header_only).unwrap();
        assert!(decrypt_file(&encrypted, &restored, "correct").is_err());
        assert!(!restored.exists());
    }

    #[test]
    fn encrypted_empty_file_is_authenticated_and_existing_files_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let encrypted = root.path().join("encrypted");
        let restored = root.path().join("restored");
        fs::write(&source, b"").unwrap();
        encrypt_file(&source, &encrypted, "correct").unwrap();
        assert!(decrypt_file(&encrypted, &restored, "wrong").is_err());
        decrypt_file(&encrypted, &restored, "correct").unwrap();
        assert_eq!(fs::metadata(&restored).unwrap().len(), 0);
        fs::write(&restored, b"keep me").unwrap();
        assert!(decrypt_file(&encrypted, &restored, "correct").is_err());
        assert!(encrypt_file(&source, &restored, "correct").is_err());
        assert_eq!(fs::read(&restored).unwrap(), b"keep me");
    }

    #[test]
    fn legacy_encrypted_files_remain_readable() {
        let root = tempfile::tempdir().unwrap();
        let encrypted = root.path().join("legacy");
        let restored = root.path().join("restored");
        let salt = [1_u8; SALT_LEN];
        let nonce = [2_u8; NONCE_LEN];
        let key = Zeroizing::new(derive_key("password", &salt).unwrap());
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).unwrap();
        let payload = b"legacy contents";
        let ciphertext = cipher
            .encrypt(
                &XNonce::from(chunk_nonce(nonce, 0)),
                Payload {
                    msg: payload,
                    aad: &0_u64.to_le_bytes(),
                },
            )
            .unwrap();
        let mut bytes = LEGACY_MAGIC.to_vec();
        bytes.extend_from_slice(&salt);
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&(DEFAULT_CHUNK_SIZE as u32).to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&ciphertext);
        fs::write(&encrypted, &bytes).unwrap();
        decrypt_file(&encrypted, &restored, "password").unwrap();
        assert_eq!(fs::read(&restored).unwrap(), payload);
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let root = std::env::temp_dir().join(format!("nuvio-crypto-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let input = root.join("source.bin");
        let encrypted = root.join("source.nuv");
        let decrypted = root.join("restored.bin");
        let payload = vec![42_u8; DEFAULT_CHUNK_SIZE + 913];
        fs::write(&input, &payload).unwrap();

        encrypt_file(&input, &encrypted, "correct horse battery staple").unwrap();
        decrypt_file(&encrypted, &decrypted, "correct horse battery staple").unwrap();

        assert_eq!(fs::read(&decrypted).unwrap(), payload);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sha256_is_stable() {
        let path = std::env::temp_dir().join(format!("nuvio-hash-{}.txt", std::process::id()));
        fs::write(&path, b"nuvio").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "83f41b253eebe749bd32512d0985d2562207126cf1c10b1f8dabac552ed7f061"
        );
        let _ = fs::remove_file(path);
    }
}
