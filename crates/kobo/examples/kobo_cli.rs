//! Tries the Kobo client against a real account, using the login of TnS-hun's kobo-book-downloader.
//!
//! ```sh
//! cargo run --example kobo_cli -- list [--all]
//! cargo run --example kobo_cli -- get <book-id> <output-dir>
//! ```
//!
//! Reads the credentials from `~/.config/kobo-book-downloader.json`, or the file in `KOBO_SETTINGS`. Refreshed tokens
//! are written back to that file, so the Python tool keeps working after Kobo rotates the refresh token.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kobo::{BookKind, Credentials, KoboClient};
use serde_json::{Map, Value};

type FieldAccessor = fn(&mut Credentials) -> &mut String;

/// The credential fields in the Python tool's settings file and our matching fields.
const FIELDS: [(&str, FieldAccessor); 6] = [
    ("DeviceId", |c| &mut c.device_id),
    ("SerialNumber", |c| &mut c.serial_number),
    ("AccessToken", |c| &mut c.access_token),
    ("RefreshToken", |c| &mut c.refresh_token),
    ("UserId", |c| &mut c.user_id),
    ("UserKey", |c| &mut c.user_key),
];

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["list"] => list(false).await,
        ["list", "--all"] => list(true).await,
        ["get", book_id, output_dir] => get(book_id, Path::new(output_dir)).await,
        _ => Err("usage: kobo_cli list [--all] | kobo_cli get <book-id> <output-dir>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn list(include_read: bool) -> Result<(), Box<dyn std::error::Error>> {
    let client = client()?;
    for book in client.library().await? {
        if book.read && !include_read {
            continue;
        }
        let mut notes = Vec::new();
        if book.kind == BookKind::Audiobook {
            notes.push("audiobook");
        }
        if book.archived {
            notes.push("archived");
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!(" [{}]", notes.join(", "))
        };
        println!("{}  {} - {}{notes}", book.revision_id, book.title, book.author);
    }
    Ok(())
}

async fn get(book_id: &str, output_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let client = client()?;
    let book = client
        .library()
        .await?
        .into_iter()
        .find(|book| book.revision_id == book_id)
        .ok_or("no book with that ID in the library")?;
    let destination = output_dir.join(book.file_name());
    client.download_book(&book.revision_id, &destination).await?;
    println!("Wrote {}", destination.display());
    Ok(())
}

fn client() -> Result<KoboClient, Box<dyn std::error::Error>> {
    let settings_path = settings_path()?;
    let settings: Map<String, Value> = serde_json::from_str(&std::fs::read_to_string(&settings_path)?)?;

    let mut credentials = Credentials::default();
    for (key, field) in FIELDS {
        *field(&mut credentials) = settings.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    }
    if !credentials.is_logged_in() {
        return Err(format!("not logged in according to {}", settings_path.display()).into());
    }

    Ok(
        KoboClient::new(credentials)?.on_credentials_changed(move |credentials| {
            if let Err(error) = save_credentials(&settings_path, credentials) {
                eprintln!(
                    "warning: couldn't save the refreshed tokens to {}: {error}",
                    settings_path.display()
                );
            }
        }),
    )
}

fn save_credentials(path: &Path, credentials: &Credentials) -> Result<(), Box<dyn std::error::Error>> {
    // Keep any other settings the Python tool stores in the file.
    let mut settings: Map<String, Value> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let mut credentials = credentials.clone();
    for (key, field) in FIELDS {
        settings.insert(key.to_owned(), Value::String(std::mem::take(field(&mut credentials))));
    }
    std::fs::write(path, serde_json::to_string_pretty(&settings)?)?;
    Ok(())
}

fn settings_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = std::env::var_os("KOBO_SETTINGS") {
        return Ok(path.into());
    }
    let config_dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME isn't set")?).join(".config"),
    };
    Ok(config_dir.join("kobo-book-downloader.json"))
}
