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
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        if let Some(credentials) = credentials {
            CredentialsStore::new(data_dir.path()).save(&credentials).unwrap();
        }
        let base = Url::parse(&format!("{}/", server.uri())).unwrap();
        let app = kobodl_web::app(Config {
            data_dir: data_dir.path().to_owned(),
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

    fn stored_credentials(&self) -> Credentials {
        CredentialsStore::new(self.data_dir.path()).load().unwrap()
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
    app.mount_library(json!([book("rev-1", "Kniv", false, false)])).await;
    Mock::given(method("GET"))
        .and(path("/v1/products/books/rev-1/access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ContentUrls": [{
                "DRMType": "SignedNoDrm",
                "UrlFormat": "EPUB3",
                "DownloadUrl": format!("{}/files/rev-1", app.server.uri()),
            }],
        })))
        .mount(&app.server)
        .await;
    let epub = epub();
    Mock::given(method("GET"))
        .and(path("/files/rev-1"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(epub.clone()))
        .mount(&app.server)
        .await;

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
