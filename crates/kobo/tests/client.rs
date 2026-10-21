//! Tests for `KoboClient` against a fake Kobo API.

use std::io::{Cursor, Read, Write};
use std::sync::{Arc, Mutex};

use aes::Aes128;
use aes::cipher::{BlockCipherEncrypt, KeyInit};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use kobo::{ActivationStatus, Credentials, Endpoints, KoboClient};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const DEVICE_ID: &str = "device-1";
const USER_ID: &str = "user-1";

fn logged_in_credentials() -> Credentials {
    Credentials {
        device_id: DEVICE_ID.to_owned(),
        serial_number: "serial-1".to_owned(),
        access_token: "access-1".to_owned(),
        refresh_token: "refresh-1".to_owned(),
        user_id: USER_ID.to_owned(),
        user_key: "user-key-1".to_owned(),
        email: "reader@example.com".to_owned(),
    }
}

/// Starts a fake Kobo API. Returns the server and a client that records every credentials change.
async fn setup(credentials: Credentials) -> (MockServer, KoboClient, Arc<Mutex<Vec<Credentials>>>) {
    let server = MockServer::start().await;
    let base = Url::parse(&format!("{}/", server.uri())).unwrap();
    let saved = Arc::new(Mutex::new(Vec::new()));
    let saved_by_listener = saved.clone();
    let client = KoboClient::new(credentials)
        .unwrap()
        .with_endpoints(Endpoints {
            store_api: base.clone(),
            auth: base,
        })
        .on_credentials_changed(move |credentials| saved_by_listener.lock().unwrap().push(credentials.clone()));
    (server, client, saved)
}

async fn mount_initialization(server: &MockServer, access_token: &str) {
    Mock::given(method("GET"))
        .and(path("/v1/initialization"))
        .and(header("authorization", format!("Bearer {access_token}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Resources": {
                "library_sync": format!("{}/v1/library/sync", server.uri()),
                "content_access_book": format!("{}/v1/products/books/{{ProductId}}/access", server.uri()),
            }
        })))
        .mount(server)
        .await;
}

fn auth_response(access_token: &str, refresh_token: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "TokenType": "Bearer",
        "AccessToken": access_token,
        "RefreshToken": refresh_token,
        "UserKey": "user-key-from-device-auth",
    }))
}

fn library_entry(revision_id: &str, title: &str) -> Value {
    json!({
        "NewEntitlement": {
            "BookEntitlement": { "Accessibility": "Full" },
            "BookMetadata": { "RevisionId": revision_id, "Title": title, "ContributorRoles": [] },
        }
    })
}

#[tokio::test]
async fn refreshes_an_expired_token_once_and_retries() {
    let (server, client, saved) = setup(logged_in_credentials()).await;

    Mock::given(method("GET"))
        .and(path("/v1/initialization"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/refresh"))
        .and(header("authorization", "Bearer access-1"))
        .and(body_partial_json(json!({ "RefreshToken": "refresh-1" })))
        .respond_with(auth_response("access-2", "refresh-2"))
        .expect(1)
        .mount(&server)
        .await;
    mount_initialization(&server, "access-2").await;
    Mock::given(method("GET"))
        .and(path("/v1/library/sync"))
        .and(header("authorization", "Bearer access-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    client.library().await.unwrap();

    let saved = saved.lock().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].access_token, "access-2");
    assert_eq!(saved[0].refresh_token, "refresh-2");
    assert_eq!(saved[0].user_key, "user-key-1");
}

#[tokio::test]
async fn follows_library_sync_tokens() {
    let (server, client, _) = setup(logged_in_credentials()).await;
    mount_initialization(&server, "access-1").await;

    Mock::given(method("GET"))
        .and(path("/v1/library/sync"))
        .and(|request: &Request| !request.headers.contains_key("x-kobo-synctoken"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-kobo-sync", "continue")
                .insert_header("x-kobo-synctoken", "token-2")
                .set_body_json(json!([library_entry("rev-b", "Second")])),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/library/sync"))
        .and(header("x-kobo-synctoken", "token-2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-kobo-synctoken", "token-3")
                .set_body_json(json!([library_entry("rev-a", "First")])),
        )
        .expect(1)
        .mount(&server)
        .await;

    let books = client.library().await.unwrap();

    let titles: Vec<_> = books.iter().map(|book| book.title.as_str()).collect();
    assert_eq!(titles, ["First", "Second"]);
}

#[tokio::test]
async fn logs_in_with_web_activation() {
    let (server, client, saved) = setup(Credentials::default()).await;

    // Device registration without a user key, before the activation starts.
    Mock::given(method("POST"))
        .and(path("/v1/auth/device"))
        .and(|request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            body.get("UserKey").is_none() && body["DeviceId"].as_str().unwrap().len() == 64
        })
        .respond_with(auth_response("anonymous-access", "anonymous-refresh"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ActivateOnWeb"))
        .and(query_param("wsa", "Kobo"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<div data-poll-endpoint="/ActivateOnWeb/Poll?id=abc&amp;s=1"></div>
               <img src="/qrcodegenerator/generate?text=https%3A%2F%2Fkobo.com%2Factivate%3Fx%3D1%26code%3D654321">"#,
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ActivateOnWeb/Poll"))
        .and(query_param("id", "abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "Status": "Pending" })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ActivateOnWeb/Poll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Status": "Complete",
            "RedirectUrl": "kobo://UserAuthenticated?returnUrl=https%3A%2F%2Fwww.kobo.com&userKey=real-user-key&userId=user-42&email=reader%40example.com",
        })))
        .mount(&server)
        .await;
    // Device registration with the user key from the activation.
    Mock::given(method("POST"))
        .and(path("/v1/auth/device"))
        .and(body_partial_json(json!({ "UserKey": "real-user-key" })))
        .respond_with(auth_response("user-access", "user-refresh"))
        .expect(1)
        .mount(&server)
        .await;

    let activation = client.start_activation().await.unwrap();
    assert_eq!(activation.code, "654321");
    assert_eq!(
        client.poll_activation(&activation).await.unwrap(),
        ActivationStatus::Pending
    );
    assert_eq!(
        client.poll_activation(&activation).await.unwrap(),
        ActivationStatus::Complete
    );

    let credentials = client.credentials();
    assert!(credentials.is_logged_in());
    assert_eq!(credentials.user_id, "user-42");
    assert_eq!(credentials.email, "reader@example.com");
    assert_eq!(credentials.user_key, "user-key-from-device-auth");
    assert_eq!(credentials.access_token, "user-access");
    assert_eq!(credentials.serial_number.len(), 32);
    // The device ID stays the same through the login.
    let saved = saved.lock().unwrap();
    assert!(saved.iter().all(|saved| saved.device_id == credentials.device_id));
    assert_eq!(saved.last().unwrap(), &credentials);
}

fn ecb_encrypt(key: &[u8; 16], data: &mut [u8]) {
    let cipher = Aes128::new(&(*key).into());
    for block in data.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(block.into());
    }
}

/// Builds a minimal EPUB with one chapter encrypted the way Kobo does it. Returns the EPUB and the content key for
/// the chapter.
fn encrypted_epub(chapter: &[u8]) -> (Vec<u8>, String) {
    let content_key = [7u8; 16];

    let padding = 16 - chapter.len() % 16;
    let mut encrypted_chapter = chapter.to_vec();
    encrypted_chapter.resize(chapter.len() + padding, padding as u8);
    ecb_encrypt(&content_key, &mut encrypted_chapter);

    let digest = Sha256::digest(format!("{DEVICE_ID}{USER_ID}"));
    let mut encrypted_content_key = content_key;
    ecb_encrypt(&digest[16..].try_into().unwrap(), &mut encrypted_content_key);

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, contents) in [
        ("mimetype", &b"application/epub+zip"[..]),
        ("META-INF/container.xml", b"<container/>"),
        ("chapter.xhtml", &encrypted_chapter),
    ] {
        writer.start_file(name, SimpleFileOptions::default()).unwrap();
        writer.write_all(contents).unwrap();
    }
    (
        writer.finish().unwrap().into_inner(),
        BASE64.encode(encrypted_content_key),
    )
}

#[tokio::test]
async fn downloads_and_decrypts_a_book() {
    let (server, client, _) = setup(logged_in_credentials()).await;
    mount_initialization(&server, "access-1").await;

    let chapter = b"<p>It was a dark and stormy night.</p>";
    let (epub, content_key) = encrypted_epub(chapter);

    Mock::given(method("GET"))
        .and(path("/v1/products/books/rev-1/access"))
        .and(query_param("DisplayProfile", "Android"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ContentKeys": [{ "Name": "chapter.xhtml", "Value": content_key }],
            "ContentUrls": [
                { "DRMType": "AdobeDrm", "UrlFormat": "EPUB3", "DownloadUrl": format!("{}/adobe", server.uri()) },
                { "DRMType": "KDRM", "UrlFormat": "EPUB3", "DownloadUrl": format!("{}/files/rev-1?b=x&token=t", server.uri()) },
            ],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/files/rev-1"))
        .and(query_param("token", "t"))
        .and(|request: &Request| request.url.query_pairs().all(|(key, _)| key != "b"))
        .and(header("x-amz-request-payer", "requester"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(epub))
        .expect(1)
        .mount(&server)
        .await;

    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("book.epub");
    client.download_book("rev-1", &destination).await.unwrap();

    let mut archive = zip::ZipArchive::new(std::fs::File::open(&destination).unwrap()).unwrap();
    let mut decrypted = Vec::new();
    archive
        .by_name("chapter.xhtml")
        .unwrap()
        .read_to_end(&mut decrypted)
        .unwrap();
    assert_eq!(decrypted, chapter);
    // Only the book is left in the directory, no temporary files.
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    // Other users, like a program watching the folder, can read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&destination).unwrap().permissions().mode();
        assert_eq!(mode & 0o444, 0o444, "mode {mode:o}");
    }
}

#[tokio::test]
async fn failed_download_leaves_nothing_behind() {
    let (server, client, _) = setup(logged_in_credentials()).await;
    mount_initialization(&server, "access-1").await;

    Mock::given(method("GET"))
        .and(path("/v1/products/books/rev-1/access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ContentUrls": [{ "DRMType": "SignedNoDrm", "UrlFormat": "EPUB3", "DownloadUrl": format!("{}/files/rev-1", server.uri()) }],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/files/rev-1"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"not a zip file".to_vec()))
        .mount(&server)
        .await;

    let directory = tempfile::tempdir().unwrap();
    let error = client
        .download_book("rev-1", &directory.path().join("book.epub"))
        .await
        .unwrap_err();

    assert!(
        matches!(error, kobo::KoboError::CorruptBook(_)),
        "unexpected error: {error}"
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn download_requires_login() {
    let (_server, client, _) = setup(Credentials::default()).await;
    let error = client
        .download_book("rev-1", std::path::Path::new("book.epub"))
        .await
        .unwrap_err();
    assert!(matches!(error, kobo::KoboError::NotLoggedIn));
}
