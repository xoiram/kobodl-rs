use std::io::{Read, Seek};

use zip::ZipArchive;

/// Checks that a file is a readable EPUB: a valid ZIP archive whose entries all pass their CRC-32 check, with a
/// `META-INF/container.xml`. Returns a description of the problem otherwise.
///
/// The same check as `CheckEpubFile` in TnS-hun's kobo-book-downloader.
pub fn check_epub<R: Read + Seek>(reader: R) -> Result<(), String> {
    let mut archive = ZipArchive::new(reader).map_err(|error| format!("not a valid ZIP archive ({error})"))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let name = entry.name().map(|name| name.into_owned()).unwrap_or_default();
        // Reading an entry to the end verifies its CRC-32.
        std::io::copy(&mut entry, &mut std::io::sink())
            .map_err(|error| format!("can't read '{name}' inside the archive ({error})"))?;
    }
    if archive.index_for_name("META-INF/container.xml").is_none() {
        return Err("META-INF/container.xml is missing, the file is not an EPUB".to_owned());
    }
    Ok(())
}
