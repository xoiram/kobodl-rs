//! Tests for the web app against a fake Kobo API.

use std::io::{Cursor, Write};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use kobo::{Credentials, Endpoints};
use kobodl_web::{Config, CredentialsStore};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use url::Url;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct TestApp {
    app: Router,
    server: MockServer,
    data_dir: TempDir,
}

impl TestApp {
    async fn start(credentials: Option<Credentials>) -> Self {
        Self::start_with_output_dir(credentials, None).await
    }

    async fn start_with_output_dir(credentials: Option<Credentials>, output_dir: Option<&std::path::Path>) -> Self {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        if let Some(credentials) = credentials {
            CredentialsStore::new(data_dir.path()).save(&credentials).unwrap();
        }
        let base = Url::parse(&format!("{}/", server.uri())).unwrap();
        let app = kobodl_web::app(Config {
            data_dir: data_dir.path().to_owned(),
            output_dir: output_dir.map(ToOwned::to_owned),
            endpoints: Endpoints {
                store_api: base.clone(),
                auth: base,
            },
        })
        .unwrap();

        Mock::given(method("GET"))
            .and(path("/v1/initialization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Resources": {
                    "library_sync": format!("{}/v1/library/sync", server.uri()),
                    "content_access_book": format!("{}/v1/products/books/{{ProductId}}/access", server.uri()),
                }
            })))
            .mount(&server)
            .await;

        Self { app, server, data_dir }
    }

    async fn get(&self, uri: &str) -> Response {
        let request = Request::get(uri).body(Body::empty()).unwrap();
        self.app.clone().oneshot(request).await.unwrap()
    }

    async fn post(&self, uri: &str, sec_fetch_site: &str) -> Response {
        let request = Request::post(uri)
            .header("sec-fetch-site", sec_fetch_site)
            .body(Body::empty())
            .unwrap();
        self.app.clone().oneshot(request).await.unwrap()
    }

    async fn post_form(&self, uri: &str, fields: &[(&str, &str)]) -> Response {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        let request = Request::post(uri)
            .header("sec-fetch-site", "same-origin")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap();
        self.app.clone().oneshot(request).await.unwrap()
    }

    fn stored_credentials(&self) -> Credentials {
        CredentialsStore::new(self.data_dir.path()).load().unwrap()
    }

    /// Serves a library with one DRM-free book, and the book itself. Returns the book's EPUB.
    async fn mount_book_download(&self, revision_id: &str, title: &str) -> Vec<u8> {
        self.mount_library(json!([book(revision_id, title, false, false)]))
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/products/books/{revision_id}/access")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ContentUrls": [{
                    "DRMType": "SignedNoDrm",
                    "UrlFormat": "EPUB3",
                    "DownloadUrl": format!("{}/files/{revision_id}", self.server.uri()),
                }],
            })))
            .mount(&self.server)
            .await;
        let epub = epub();
        Mock::given(method("GET"))
            .and(path(format!("/files/{revision_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(epub.clone()))
            .mount(&self.server)
            .await;
        epub
    }

    async fn mount_library(&self, entries: Value) {
        Mock::given(method("GET"))
            .and(path("/v1/library/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_json(entries))
            .mount(&self.server)
            .await;
    }
}

fn logged_in() -> Credentials {
    Credentials {
        device_id: "device-1".to_owned(),
        serial_number: "serial-1".to_owned(),
        access_token: "access-1".to_owned(),
        refresh_token: "refresh-1".to_owned(),
        user_id: "user-1".to_owned(),
        user_key: "key-1".to_owned(),
        email: "reader@example.com".to_owned(),
    }
}

fn book(revision_id: &str, title: &str, archived: bool, read: bool) -> Value {
    json!({
        "NewEntitlement": {
            "BookEntitlement": { "Accessibility": "Full", "IsRemoved": archived },
            "BookMetadata": {
                "RevisionId": revision_id,
                "Title": title,
                "ContributorRoles": [{ "Name": "Jo Nesbø" }],
            },
            "ReadingState": { "StatusInfo": { "Status": if read { "Finished" } else { "ReadyToRead" } } },
        }
    })
}

fn location(response: &Response) -> &str {
    response.headers()[header::LOCATION].to_str().unwrap()
}

async fn body_text(response: Response) -> String {
    String::from_utf8(to_bytes(response.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap()
}

fn auth_response(access_token: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "TokenType": "Bearer",
        "AccessToken": access_token,
        "RefreshToken": format!("{access_token}-refresh"),
        "UserKey": "key-from-kobo",
    }))
}

#[tokio::test]
async fn redirects_to_login_when_logged_out() {
    let app = TestApp::start(None).await;

    let response = app.get("/").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/login");

    let response = app.get("/library").await;
    assert_eq!(location(&response), "/login");
}

#[tokio::test]
async fn logs_in_with_an_activation_code() {
    let app = TestApp::start(None).await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/device"))
        .respond_with(auth_response("anonymous"))
        .up_to_n_times(1)
        .mount(&app.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/device"))
        .and(body_partial_json(json!({ "UserKey": "real-key" })))
        .respond_with(auth_response("user"))
        .with_priority(1)
        .mount(&app.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ActivateOnWeb"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<div data-poll-endpoint="/ActivateOnWeb/Poll"></div>
               <img src="/qrcodegenerator/generate?text=x%26code%3D246810">"#,
        ))
        .mount(&app.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ActivateOnWeb/Poll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "Status": "Pending" })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&app.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ActivateOnWeb/Poll"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Status": "Complete",
            "RedirectUrl": "kobo://UserAuthenticated?userKey=real-key&userId=user-9&email=reader%40example.com",
        })))
        .mount(&app.server)
        .await;

    let page = body_text(app.get("/login").await).await;
    assert!(page.contains("Get a code"));

    let response = app.post("/login/start", "same-origin").await;
    assert_eq!(location(&response), "/login");

    let response = app.get("/login").await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = body_text(response).await;
    assert!(page.contains("246810"));
    assert!(page.contains(r#"http-equiv="refresh""#));

    let response = app.get("/login").await;
    assert_eq!(location(&response), "/library");

    let stored = app.stored_credentials();
    assert!(stored.is_logged_in());
    assert_eq!(stored.user_id, "user-9");
    assert_eq!(stored.access_token, "user");
}

#[tokio::test]
async fn rejects_cross_site_posts() {
    let app = TestApp::start(Some(logged_in())).await;

    let response = app.post("/logout", "cross-site").await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(app.stored_credentials(), logged_in());
}

#[tokio::test]
async fn logout_keeps_the_device_registration() {
    let app = TestApp::start(Some(logged_in())).await;

    let response = app.post("/logout", "same-origin").await;

    assert_eq!(location(&response), "/login");
    let stored = app.stored_credentials();
    assert!(!stored.is_logged_in());
    assert_eq!(stored.device_id, "device-1");
    assert_eq!(stored.serial_number, "serial-1");
    assert!(stored.access_token.is_empty());
    assert_eq!(location(&app.get("/").await), "/login");
}

#[tokio::test]
async fn library_page_filters_and_escapes_books() {
    let app = TestApp::start(Some(logged_in())).await;
    app.mount_library(json!([
        book("rev-1", "<b>Kniv</b>", false, false),
        book("rev-2", "Already read", false, true),
        book("rev-3", "Archived one", true, false),
    ]))
    .await;

    let page = body_text(app.get("/library").await).await;
    assert!(page.contains("&#60;b&#62;Kniv&#60;/b&#62;"));
    assert!(!page.contains("<b>Kniv"));
    assert!(page.contains("/books/rev-1/epub"));
    assert!(page.contains("Jo Nesbø"));
    assert!(!page.contains("Already read"));
    assert!(!page.contains("Archived one"));
    assert!(page.contains("2 books are hidden"));

    let page = body_text(app.get("/library?read=true&archived=true").await).await;
    assert!(page.contains("Already read"));
    assert!(page.contains("Archived one"));
    assert!(
        !page.contains("/books/rev-3/epub"),
        "archived books can't be downloaded"
    );
}

fn epub() -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, contents) in [
        ("mimetype", "application/epub+zip"),
        ("META-INF/container.xml", "<container/>"),
    ] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[tokio::test]
async fn downloads_a_book() {
    let app = TestApp::start(Some(logged_in())).await;
    let epub = app.mount_book_download("rev-1", "Kniv").await;

    let response = app.get("/books/rev-1/epub").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/epub+zip");
    let disposition = response.headers()[header::CONTENT_DISPOSITION].to_str().unwrap();
    assert!(
        disposition.contains("filename*=UTF-8''Jo%20Nesb%C3%B8%20%2D%20Kniv%20rev%2D1%2Eepub"),
        "{disposition}"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), epub.as_slice());
}

#[tokio::test]
async fn unknown_book_is_not_found() {
    let app = TestApp::start(Some(logged_in())).await;
    app.mount_library(json!([book("rev-1", "Kniv", false, false)])).await;

    let response = app.get("/books/nope/epub").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn kobo_errors_are_shown_as_a_page() {
    let app = TestApp::start(Some(logged_in())).await;
    app.mount_library(json!([book("rev-1", "Kniv", false, false)])).await;
    Mock::given(method("GET"))
        .and(path("/v1/products/books/rev-1/access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ContentUrls": [] })))
        .mount(&app.server)
        .await;

    let response = app.get("/books/rev-1/epub").await;

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(body_text(response).await.contains("Archived books have to be restored"));
}

fn python_settings() -> String {
    json!({
        "AccessToken": "imported-access",
        "DeviceId": "imported-device",
        "RefreshToken": "imported-refresh",
        "SerialNumber": "imported-serial",
        "UserId": "imported-user",
        "UserKey": "imported-key",
    })
    .to_string()
}

#[tokio::test]
async fn imports_a_login_and_refreshes_its_tokens() {
    let app = TestApp::start(None).await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/refresh"))
        .and(wiremock::matchers::header("authorization", "Bearer imported-access"))
        .and(body_partial_json(json!({ "RefreshToken": "imported-refresh" })))
        .respond_with(auth_response("fresh"))
        .expect(1)
        .mount(&app.server)
        .await;

    let response = app
        .post_form("/login/import", &[("settings", &python_settings())])
        .await;

    assert_eq!(location(&response), "/library");
    let stored = app.stored_credentials();
    assert!(stored.is_logged_in());
    assert_eq!(stored.device_id, "imported-device");
    assert_eq!(stored.user_id, "imported-user");
    assert_eq!(stored.access_token, "fresh");
    assert_eq!(stored.refresh_token, "fresh-refresh");
}

#[tokio::test]
async fn rejected_import_saves_nothing() {
    let app = TestApp::start(None).await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/refresh"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&app.server)
        .await;

    let response = app
        .post_form("/login/import", &[("settings", &python_settings())])
        .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let page = body_text(response).await;
    assert!(page.contains("didn&#39;t accept the imported login"), "{page}");
    assert!(!page.contains("imported-access"), "tokens must not be echoed back");
    assert!(!app.data_dir.path().join("state.json").exists());
}

#[tokio::test]
async fn invalid_import_is_explained() {
    let app = TestApp::start(None).await;

    let response = app
        .post_form("/login/import", &[("settings", r#"{"theme": "dark"}"#)])
        .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body_text(response)
            .await
            .contains("isn&#39;t a kobo-book-downloader or kobodl settings file")
    );
}

#[tokio::test]
async fn saves_a_book_to_the_output_folder() {
    let output_dir = tempfile::tempdir().unwrap();
    let app = TestApp::start_with_output_dir(Some(logged_in()), Some(output_dir.path())).await;
    let epub = app.mount_book_download("rev-1", "Kniv").await;

    let page = body_text(app.get("/library?read=true").await).await;
    assert!(page.contains(r#"action="/books/rev-1/save""#));
    assert!(page.contains(r#"<input type="hidden" name="read" value="true">"#));

    let response = app.post_form("/books/rev-1/save", &[("read", "true")]).await;

    assert_eq!(location(&response), "/library?read=true&saved=rev%2D1");
    let saved: Vec<_> = std::fs::read_dir(output_dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(saved, ["Jo Nesbø - Kniv rev-1.epub"]);
    let path = output_dir.path().join(&saved[0]);
    assert_eq!(std::fs::read(&path).unwrap(), epub);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o444,
            0o444,
            "other users must be able to read it, mode {mode:o}"
        );
    }

    let page = body_text(app.get("/library?read=true&saved=rev-1").await).await;
    assert!(page.contains("Saved “Kniv” to the server."));
}

#[tokio::test]
async fn saving_needs_an_output_folder() {
    let app = TestApp::start(Some(logged_in())).await;
    app.mount_library(json!([book("rev-1", "Kniv", false, false)])).await;

    assert!(!body_text(app.get("/library").await).await.contains("Save to server"));
    let response = app.post_form("/books/rev-1/save", &[]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn missing_output_folder_is_a_startup_error() {
    let data_dir = tempfile::tempdir().unwrap();
    let result = kobodl_web::app(Config {
        data_dir: data_dir.path().to_owned(),
        output_dir: Some(data_dir.path().join("missing")),
        endpoints: Endpoints::default(),
    });
    assert!(result.is_err());
}
