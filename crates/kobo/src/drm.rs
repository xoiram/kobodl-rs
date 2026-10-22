//! Removal of the Kobo DRM (`KDRM`) from downloaded EPUBs.
//!
//! Ported from `koboDrmRemover.py` in kobodl (<https://github.com/subdavis/kobo-book-downloader>, Unlicense), which is
//! based on `obok.py` by Physisticated from DeDRM_tools
//! (<https://github.com/apprenticeharper/DeDRM_tools/blob/master/Other_Tools/Kobo/obok.py>). `obok.py` has no license
//! file that we could find, so see the "Prior art" section of the README for how to reach us about attribution.
//!
//! A Kobo EPUB is a ZIP archive in which some entries are encrypted with AES-128-ECB and PKCS7 padding. Each encrypted
//! entry has its own content key, which the `content_access_book` API returns, itself encrypted with AES-128-ECB under
//! a key derived from the device ID and the user ID.

use std::collections::HashMap;
use std::io::{Read, Seek, Write};

use aes::Aes128;
use aes::cipher::{Block, BlockCipherDecrypt, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const AES_BLOCK_SIZE: usize = 16;

/// The EPUB specification requires this entry to be stored uncompressed and as the first entry in the archive.
/// See <https://www.w3.org/TR/epub/#sec-zip-container-mime>.
const MIMETYPE_ENTRY: &str = "mimetype";

#[derive(Debug, thiserror::Error)]
pub enum DrmError {
    #[error("invalid EPUB archive: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("the content key for '{entry}' is not valid base64")]
    InvalidKeyEncoding {
        entry: String,
        #[source]
        source: base64::DecodeError,
    },

    #[error("the content key for '{entry}' is {length} bytes long, expected {AES_BLOCK_SIZE}")]
    InvalidKeyLength { entry: String, length: usize },

    #[error("the encrypted size of '{entry}' ({length} bytes) is not a multiple of {AES_BLOCK_SIZE}")]
    InvalidCiphertextLength { entry: String, length: usize },

    #[error("'{entry}' has invalid padding after decryption; the device ID, user ID or content key is probably wrong")]
    InvalidPadding { entry: String },
}

/// Removes the Kobo DRM from EPUBs downloaded for one device and user.
pub struct DrmRemover {
    user_key: [u8; AES_BLOCK_SIZE],
}

impl DrmRemover {
    pub fn new(device_id: &str, user_id: &str) -> Self {
        Self {
            user_key: user_key(device_id, user_id),
        }
    }

    /// Reads the encrypted EPUB from `input` and writes a decrypted copy to `output`.
    ///
    /// `content_keys` maps archive entry names to their base64-encoded, encrypted content keys. Entries without a
    /// key are copied unchanged.
    pub fn remove_drm<R, W>(&self, input: R, output: W, content_keys: &HashMap<String, String>) -> Result<W, DrmError>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let mut archive = ZipArchive::new(input)?;
        let mut writer = ZipWriter::new(output);

        // Move the mimetype entry to the front, in case the input doesn't have it there.
        let mimetype_index = archive.index_for_name(MIMETYPE_ENTRY);
        let other_indexes = (0..archive.len()).filter(|&index| Some(index) != mimetype_index);

        for index in mimetype_index.into_iter().chain(other_indexes) {
            let mut entry = archive.by_index(index)?;
            let name = entry.name()?.into_owned();

            let mut options = SimpleFileOptions::default();
            if let Some(modified) = entry.last_modified() {
                options = options.last_modified_time(modified);
            }
            if let Some(mode) = entry.unix_mode() {
                options = options.unix_permissions(mode);
            }

            if entry.is_dir() {
                writer.add_directory(name, options)?;
                continue;
            }

            let mut contents = Vec::with_capacity(entry.size().try_into().unwrap_or(0));
            entry.read_to_end(&mut contents)?;

            if let Some(content_key) = content_keys.get(&name) {
                contents = self.decrypt_contents(&name, contents, content_key)?;
            }

            let compression = if name == MIMETYPE_ENTRY {
                CompressionMethod::Stored
            } else {
                CompressionMethod::Deflated
            };
            writer.start_file(name, options.compression_method(compression))?;
            writer.write_all(&contents)?;
        }

        Ok(writer.finish()?)
    }

    fn decrypt_contents(
        &self,
        entry: &str,
        mut contents: Vec<u8>,
        content_key_base64: &str,
    ) -> Result<Vec<u8>, DrmError> {
        let mut content_key = BASE64
            .decode(content_key_base64)
            .map_err(|source| DrmError::InvalidKeyEncoding {
                entry: entry.to_owned(),
                source,
            })?;
        let length = content_key.len();
        let content_key: &mut [u8; AES_BLOCK_SIZE] =
            content_key
                .as_mut_slice()
                .try_into()
                .map_err(|_| DrmError::InvalidKeyLength {
                    entry: entry.to_owned(),
                    length,
                })?;
        ecb_decrypt_in_place(&self.user_key, content_key);

        if !contents.len().is_multiple_of(AES_BLOCK_SIZE) {
            return Err(DrmError::InvalidCiphertextLength {
                entry: entry.to_owned(),
                length: contents.len(),
            });
        }
        ecb_decrypt_in_place(content_key, &mut contents);

        let unpadded_length = pkcs7_unpadded_length(&contents).ok_or_else(|| DrmError::InvalidPadding {
            entry: entry.to_owned(),
        })?;
        contents.truncate(unpadded_length);
        Ok(contents)
    }
}

/// Derives the key that the content keys are encrypted with: the last 16 bytes of `sha256(device_id + user_id)`.
fn user_key(device_id: &str, user_id: &str) -> [u8; AES_BLOCK_SIZE] {
    let digest = Sha256::new().chain_update(device_id).chain_update(user_id).finalize();
    let mut key = [0; AES_BLOCK_SIZE];
    key.copy_from_slice(&digest[AES_BLOCK_SIZE..]);
    key
}

/// Decrypts `data` with AES-128-ECB. The length of `data` must be a multiple of the block size.
fn ecb_decrypt_in_place(key: &[u8; AES_BLOCK_SIZE], data: &mut [u8]) {
    let cipher = Aes128::new(&(*key).into());
    let (blocks, _) = data.as_chunks_mut::<AES_BLOCK_SIZE>();
    for block in blocks {
        let block: &mut Block<Aes128> = block.into();
        cipher.decrypt_block(block);
    }
}

/// Returns the length of `data` without its PKCS7 padding, or `None` if the padding is invalid.
fn pkcs7_unpadded_length(data: &[u8]) -> Option<usize> {
    let padding_length = usize::from(*data.last()?);
    if padding_length == 0 || padding_length > AES_BLOCK_SIZE || padding_length > data.len() {
        return None;
    }
    let (unpadded, padding) = data.split_at(data.len() - padding_length);
    padding
        .iter()
        .all(|&byte| usize::from(byte) == padding_length)
        .then_some(unpadded.len())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use aes::cipher::BlockCipherEncrypt;

    use super::*;

    const DEVICE_ID: &str = "0123456789abcdef";
    const USER_ID: &str = "user-1";

    // Known answers computed with the Python implementation and pycryptodome, for DEVICE_ID and USER_ID above and the
    // content key 00 01 02 ... 0f.
    const PYTHON_USER_KEY_HEX: &str = "c42ccd2703039ef67c922a8ebe6883fb";
    const PYTHON_ENCRYPTED_CONTENT_KEY: &str = "7SaPLWqDkKNwAGPWly9Bnw==";
    const PYTHON_PLAINTEXT: &[u8] = b"<p>Hello, Kobo!</p>";
    const PYTHON_CIPHERTEXT_HEX: &str = "64dbadd37943a73780acf995f99db9d4ea4a8a7214fd944ef9de0797f4c0d0bc";

    const CONTENT_KEY: [u8; AES_BLOCK_SIZE] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    fn from_hex(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    fn ecb_encrypt_in_place(key: &[u8; AES_BLOCK_SIZE], data: &mut [u8]) {
        let cipher = Aes128::new(&(*key).into());
        for block in data.as_chunks_mut::<AES_BLOCK_SIZE>().0 {
            cipher.encrypt_block(block.into());
        }
    }

    fn encrypt_contents(content_key: &[u8; AES_BLOCK_SIZE], plaintext: &[u8]) -> Vec<u8> {
        let padding_length = AES_BLOCK_SIZE - plaintext.len() % AES_BLOCK_SIZE;
        let mut data = plaintext.to_vec();
        data.resize(plaintext.len() + padding_length, padding_length as u8);
        ecb_encrypt_in_place(content_key, &mut data);
        data
    }

    fn encrypt_content_key(content_key: &[u8; AES_BLOCK_SIZE]) -> String {
        let mut key = *content_key;
        ecb_encrypt_in_place(&user_key(DEVICE_ID, USER_ID), &mut key);
        BASE64.encode(key)
    }

    enum FixtureEntry<'a> {
        File(&'a str, &'a [u8]),
        Directory(&'a str),
    }

    fn build_zip(entries: &[FixtureEntry]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for entry in entries {
            match entry {
                FixtureEntry::File(name, contents) => {
                    writer.start_file(*name, options).unwrap();
                    writer.write_all(contents).unwrap();
                }
                FixtureEntry::Directory(name) => writer.add_directory(*name, options).unwrap(),
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn read_entry(archive: &mut ZipArchive<Cursor<Vec<u8>>>, name: &str) -> Vec<u8> {
        let mut contents = Vec::new();
        archive.by_name(name).unwrap().read_to_end(&mut contents).unwrap();
        contents
    }

    fn remove_drm(
        remover: &DrmRemover,
        input: Vec<u8>,
        content_keys: &HashMap<String, String>,
    ) -> Result<ZipArchive<Cursor<Vec<u8>>>, DrmError> {
        let output = remover.remove_drm(Cursor::new(input), Cursor::new(Vec::new()), content_keys)?;
        Ok(ZipArchive::new(Cursor::new(output.into_inner())).unwrap())
    }

    #[test]
    fn user_key_matches_python_implementation() {
        assert_eq!(user_key(DEVICE_ID, USER_ID).to_vec(), from_hex(PYTHON_USER_KEY_HEX));
    }

    #[test]
    fn decrypts_contents_encrypted_by_python_implementation() {
        let remover = DrmRemover::new(DEVICE_ID, USER_ID);
        let decrypted = remover
            .decrypt_contents(
                "chapter.xhtml",
                from_hex(PYTHON_CIPHERTEXT_HEX),
                PYTHON_ENCRYPTED_CONTENT_KEY,
            )
            .unwrap();
        assert_eq!(decrypted, PYTHON_PLAINTEXT);
    }

    #[test]
    fn removes_drm_and_puts_mimetype_first_uncompressed() {
        let chapter = b"<html><body><p>Chapter one.</p></body></html>".repeat(10);
        let encrypted_chapter = encrypt_contents(&CONTENT_KEY, &chapter);
        let opf = b"<package/>";
        let input = build_zip(&[
            FixtureEntry::Directory("META-INF/"),
            FixtureEntry::File("META-INF/container.xml", b"<container/>"),
            FixtureEntry::File("OEBPS/content.opf", opf),
            FixtureEntry::File("OEBPS/chapter1.xhtml", &encrypted_chapter),
            FixtureEntry::File("mimetype", b"application/epub+zip"),
        ]);
        let content_keys = HashMap::from([
            ("OEBPS/chapter1.xhtml".to_owned(), encrypt_content_key(&CONTENT_KEY)),
            // Keys for entries that don't exist are ignored.
            ("OEBPS/missing.xhtml".to_owned(), encrypt_content_key(&CONTENT_KEY)),
        ]);

        let mut output = remove_drm(&DrmRemover::new(DEVICE_ID, USER_ID), input, &content_keys).unwrap();

        assert_eq!(output.len(), 5);
        {
            let mimetype = output.by_index(0).unwrap();
            assert_eq!(mimetype.name().unwrap(), "mimetype");
            assert_eq!(mimetype.compression(), CompressionMethod::Stored);
        }
        assert_eq!(read_entry(&mut output, "mimetype"), b"application/epub+zip");
        assert_eq!(read_entry(&mut output, "OEBPS/chapter1.xhtml"), chapter);
        assert_eq!(read_entry(&mut output, "OEBPS/content.opf"), opf);
        assert_eq!(read_entry(&mut output, "META-INF/container.xml"), b"<container/>");
        assert!(output.by_name("META-INF/").unwrap().is_dir());
        assert_eq!(
            output.by_name("OEBPS/chapter1.xhtml").unwrap().compression(),
            CompressionMethod::Deflated
        );
    }

    #[test]
    fn wrong_user_id_is_reported_as_invalid_padding() {
        let encrypted_chapter = encrypt_contents(&CONTENT_KEY, b"<p>Secret</p>");
        let input = build_zip(&[
            FixtureEntry::File("mimetype", b"application/epub+zip"),
            FixtureEntry::File("chapter.xhtml", &encrypted_chapter),
        ]);
        let content_keys = HashMap::from([("chapter.xhtml".to_owned(), encrypt_content_key(&CONTENT_KEY))]);

        let result = remove_drm(&DrmRemover::new(DEVICE_ID, "someone-else"), input, &content_keys);

        assert!(
            matches!(result, Err(DrmError::InvalidPadding { ref entry }) if entry == "chapter.xhtml"),
            "unexpected result: {:?}",
            result.err()
        );
    }

    #[test]
    fn rejects_malformed_content_keys() {
        let remover = DrmRemover::new(DEVICE_ID, USER_ID);
        let ciphertext = encrypt_contents(&CONTENT_KEY, b"text");

        let result = remover.decrypt_contents("a.xhtml", ciphertext.clone(), "not base64!");
        assert!(matches!(result, Err(DrmError::InvalidKeyEncoding { .. })));

        let result = remover.decrypt_contents("a.xhtml", ciphertext, &BASE64.encode([0u8; 8]));
        assert!(matches!(result, Err(DrmError::InvalidKeyLength { length: 8, .. })));
    }

    #[test]
    fn rejects_ciphertext_that_is_not_whole_blocks() {
        let remover = DrmRemover::new(DEVICE_ID, USER_ID);
        let result = remover.decrypt_contents("a.xhtml", vec![0; 17], &encrypt_content_key(&CONTENT_KEY));
        assert!(matches!(
            result,
            Err(DrmError::InvalidCiphertextLength { length: 17, .. })
        ));
    }

    #[test]
    fn pkcs7_unpadding() {
        assert_eq!(pkcs7_unpadded_length(&[b'a', b'b', 2, 2]), Some(2));
        assert_eq!(pkcs7_unpadded_length(&[16; 16]), Some(0));
        assert_eq!(pkcs7_unpadded_length(&[]), None);
        assert_eq!(pkcs7_unpadded_length(&[b'a', 0]), None);
        assert_eq!(pkcs7_unpadded_length(&[b'a', 1, 2]), None);
        assert_eq!(pkcs7_unpadded_length(&[17; 17]), None);
    }
}
