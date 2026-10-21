//! Removes the DRM from an encrypted Kobo EPUB, for checking the DRM code against a real book.
//!
//! Get the input files with `tools/fetch_encrypted_book.py`, then run:
//!
//! ```sh
//! cargo run --example decrypt_book -- <encrypted.epub> <keys.json> <output.epub>
//! ```

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read};
use std::process::ExitCode;

use kobo::DrmRemover;
use zip::ZipArchive;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input_path, keys_path, output_path] = args.as_slice() else {
        eprintln!("usage: decrypt_book <encrypted.epub> <keys.json> <output.epub>");
        return ExitCode::FAILURE;
    };

    match run(input_path, keys_path, output_path) {
        Ok(()) => {
            println!("Wrote {output_path}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(input_path: &str, keys_path: &str, output_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let keys: serde_json::Value = serde_json::from_reader(BufReader::new(File::open(keys_path)?))?;
    let device_id = keys["device_id"].as_str().ok_or("keys.json is missing device_id")?;
    let user_id = keys["user_id"].as_str().ok_or("keys.json is missing user_id")?;
    let content_keys: HashMap<String, String> = serde_json::from_value(keys["content_keys"].clone())?;

    let input = BufReader::new(File::open(input_path)?);
    let output = BufWriter::new(File::create(output_path)?);
    DrmRemover::new(device_id, user_id).remove_drm(input, output, &content_keys)?;

    check_epub(output_path)
}

/// The same check as `CheckEpubFile` in the Python tool: every entry passes its CRC-32 check and
/// `META-INF/container.xml` exists.
fn check_epub(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut archive = ZipArchive::new(BufReader::new(File::open(path)?))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        // Reading an entry to the end verifies its CRC-32.
        std::io::copy(&mut entry.by_ref(), &mut std::io::sink())?;
    }
    if archive.index_for_name("META-INF/container.xml").is_none() {
        return Err("META-INF/container.xml is missing, the output is not an EPUB".into());
    }
    Ok(())
}
