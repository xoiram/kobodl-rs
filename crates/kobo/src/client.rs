//! Client for the Kobo store API.
//!
//! Ported from `kobo.py` in kobodl and `Kobo.py` in TnS-hun's kobo-book-downloader. The client registers itself with
//! Kobo as an e-reader, so the constants below mimic one.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::StreamExt;
use rand::RngExt;
use regex::Regex;
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, OnceCell};
use url::Url;

use crate::drm::DrmRemover;
use crate::epub::check_epub;
use crate::error::KoboError;
use crate::library::{Book, parse_library};

const AFFILIATE: &str = "Kobo";
const APPLICATION_VERSION: &str = "4.38.23171";
const DEFAULT_PLATFORM_ID: &str = "00000000-0000-0000-0000-000000000373";
const DISPLAY_PROFILE: &str = "Android";
const DEVICE_OS: &str = "3.0.35+";
const DEVICE_OS_VERSION: &str = "NA";
const USER_AGENT: &str = "Mozilla/5.0 (Linux; U; Android 2.0; en-us;) AppleWebKit/538.1 (KHTML, like Gecko) \
                          Version/4.0 Mobile Safari/538.1 (Kobo Touch 0373/4.38.23171)";
const TIMEOUT: Duration = Duration::from_secs(30);

/// Book formats that can be decrypted into an EPUB.
const SUPPORTED_FORMATS: [&str; 4] = ["EPUB3", "EPUB3FL", "EPUB3OMF", "KEPUB"];

/// The page where the user enters the activation code from [`KoboClient::start_activation`].
pub const ACTIVATION_PAGE: &str = "https://www.kobo.com/activate";

/// The device registration and account tokens. Store these between runs; they give access to the Kobo account.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub device_id: String,
    pub serial_number: String,
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: String,
    pub user_key: String,
    pub email: String,
}

impl Credentials {
    /// Whether the device is registered with Kobo. It doesn't mean a user is logged in.
    pub fn has_device_tokens(&self) -> bool {
        !self.device_id.is_empty() && !self.access_token.is_empty() && !self.refresh_token.is_empty()
    }

    pub fn is_logged_in(&self) -> bool {
        self.has_device_tokens() && !self.user_id.is_empty() && !self.user_key.is_empty()
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("device_id", &self.device_id)
            .field("email", &self.email)
            .field("logged_in", &self.is_logged_in())
            .finish_non_exhaustive()
    }
}

/// Base URLs of the Kobo services. Only tests need to change these.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub store_api: Url,
    pub auth: Url,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            store_api: Url::parse("https://storeapi.kobo.com/").expect("valid URL"),
            auth: Url::parse("https://auth.kobobooks.com/").expect("valid URL"),
        }
    }
}

/// An activation in progress. Show `code` to the user, who enters it on [`ACTIVATION_PAGE`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activation {
    pub code: String,
    pub poll_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationStatus {
    Pending,
    Complete,
}

type CredentialsListener = Box<dyn Fn(&Credentials) + Send + Sync>;

pub struct KoboClient {
    http: reqwest::Client,
    endpoints: Endpoints,
    credentials: RwLock<Credentials>,
    /// Makes concurrent requests that get a 401 share one token refresh.
    refresh_lock: Mutex<()>,
    /// The `Resources` map from the initialization API, with the URLs of the other APIs.
    resources: OnceCell<Value>,
    on_credentials_changed: Option<CredentialsListener>,
}

impl KoboClient {
    pub fn new(credentials: Credentials) -> Result<Self, KoboError> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(TIMEOUT)
            .read_timeout(TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            endpoints: Endpoints::default(),
            credentials: RwLock::new(credentials),
            refresh_lock: Mutex::new(()),
            resources: OnceCell::new(),
            on_credentials_changed: None,
        })
    }

    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Registers a function that is called whenever the credentials change, for example when the access token is
    /// refreshed. Use it to save them.
    pub fn on_credentials_changed(mut self, listener: impl Fn(&Credentials) + Send + Sync + 'static) -> Self {
        self.on_credentials_changed = Some(Box::new(listener));
        self
    }

    pub fn credentials(&self) -> Credentials {
        self.credentials.read().expect("credentials lock poisoned").clone()
    }

    fn update_credentials(&self, update: impl FnOnce(&mut Credentials)) {
        let updated = {
            let mut credentials = self.credentials.write().expect("credentials lock poisoned");
            update(&mut credentials);
            credentials.clone()
        };
        if let Some(listener) = &self.on_credentials_changed {
            listener(&updated);
        }
    }

    /// Starts the web-based login. Registers the device first if needed.
    ///
    /// Show the returned code to the user and call [`poll_activation`](Self::poll_activation) every few seconds
    /// until it returns [`ActivationStatus::Complete`].
    pub async fn start_activation(&self) -> Result<Activation, KoboError> {
        if !self.credentials().has_device_tokens() {
            self.authenticate_device(None).await?;
        }

        let device_id = self.credentials().device_id;
        let response = self
            .http
            .get(self.endpoints.auth.join("ActivateOnWeb").expect("valid path"))
            .query(&[
                ("pwspid", DEFAULT_PLATFORM_ID),
                ("wsa", AFFILIATE),
                ("pwsdid", &device_id),
                ("pwsav", APPLICATION_VERSION),
                // The Android app sends the device model here, but the e-readers send the platform ID.
                ("pwsdm", DEFAULT_PLATFORM_ID),
                ("pwspos", DEVICE_OS),
                ("pwspov", DEVICE_OS_VERSION),
            ])
            .send()
            .await?
            .error_for_status()?;
        parse_activation_page(&self.endpoints.auth, &response.text().await?)
    }

    /// Checks whether the user has entered the activation code. When they have, finishes the login.
    pub async fn poll_activation(&self, activation: &Activation) -> Result<ActivationStatus, KoboError> {
        let response = self.http.post(&activation.poll_url).send().await?.error_for_status()?;
        let body = response.text().await?;
        let status: Value = serde_json::from_str(&body).map_err(|_| {
            tracing::debug!(body, "activation check response");
            KoboError::UnexpectedResponse("the activation check didn't return JSON".to_owned())
        })?;
        if status["Status"] != "Complete" {
            return Ok(ActivationStatus::Pending);
        }

        // RedirectUrl looks like this:
        // kobo://UserAuthenticated?returnUrl=https%3A%2F%2Fwww.kobo.com%2Fww%2Fen%2F&userKey=...&userId=...&email=...
        let redirect_url = status["RedirectUrl"]
            .as_str()
            .and_then(|url| Url::parse(url).ok())
            .ok_or_else(|| KoboError::UnexpectedResponse("the activation check has no valid RedirectUrl".to_owned()))?;
        let parameter = |name: &str| {
            redirect_url
                .query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        let missing = |name: &str| KoboError::UnexpectedResponse(format!("the activation check didn't return {name}"));
        let user_id = parameter("userId").ok_or_else(|| missing("userId"))?;
        let user_key = parameter("userKey").ok_or_else(|| missing("userKey"))?;
        let email = parameter("email").unwrap_or_default();

        self.update_credentials(|credentials| {
            credentials.user_id = user_id;
            credentials.email = email;
        });
        self.authenticate_device(Some(&user_key)).await?;
        Ok(ActivationStatus::Complete)
    }

    /// Registers the device, generating a new device ID if there is none. With a user key from the activation, this
    /// also links the device to the user.
    async fn authenticate_device(&self, user_key: Option<&str>) -> Result<(), KoboError> {
        let mut credentials = self.credentials();
        if credentials.device_id.is_empty() {
            credentials.device_id = random_hex(32);
            credentials.serial_number = random_hex(16);
        }

        let mut body = json!({
            "AffiliateName": AFFILIATE,
            "AppVersion": APPLICATION_VERSION,
            "ClientKey": client_key(),
            "DeviceId": credentials.device_id,
            "PlatformId": DEFAULT_PLATFORM_ID,
            "SerialNumber": credentials.serial_number,
        });
        if let Some(user_key) = user_key {
            body["UserKey"] = user_key.into();
        }

        let response: AuthResponse = self
            .http
            .post(self.endpoints.store_api.join("v1/auth/device").expect("valid path"))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        response.check()?;

        // The response to a request without a user key contains a user key that can't be used for anything.
        let new_user_key = match user_key {
            Some(_) => Some(response.user_key.ok_or_else(|| {
                KoboError::UnexpectedResponse("the device authentication didn't return a user key".to_owned())
            })?),
            None => None,
        };

        self.update_credentials(|stored| {
            stored.device_id = credentials.device_id;
            stored.serial_number = credentials.serial_number;
            stored.access_token = response.access_token;
            stored.refresh_token = response.refresh_token;
            if let Some(new_user_key) = new_user_key {
                stored.user_key = new_user_key;
            }
        });
        Ok(())
    }

    /// Refreshes the access token, unless another request already replaced `stale_access_token`.
    async fn refresh_authentication(&self, stale_access_token: &str) -> Result<(), KoboError> {
        let _guard = self.refresh_lock.lock().await;
        let credentials = self.credentials();
        if credentials.access_token != stale_access_token {
            return Ok(());
        }

        tracing::debug!("refreshing the Kobo access token");
        let response: AuthResponse = self
            .http
            .post(self.endpoints.store_api.join("v1/auth/refresh").expect("valid path"))
            .bearer_auth(&credentials.access_token)
            .json(&json!({
                "AppVersion": APPLICATION_VERSION,
                "ClientKey": client_key(),
                "PlatformId": DEFAULT_PLATFORM_ID,
                "RefreshToken": credentials.refresh_token,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        response.check()?;

        self.update_credentials(|stored| {
            stored.access_token = response.access_token;
            stored.refresh_token = response.refresh_token;
        });
        Ok(())
    }

    /// Sends a request with the access token. If Kobo says the token has expired, refreshes it and retries once.
    ///
    /// `build` is called again for the retry, so it must build the same request each time.
    async fn send_authenticated(
        &self,
        build: impl Fn(&reqwest::Client) -> RequestBuilder,
    ) -> Result<Response, KoboError> {
        let access_token = self.credentials().access_token;
        if access_token.is_empty() {
            return Err(KoboError::NotLoggedIn);
        }

        let response = build(&self.http).bearer_auth(&access_token).send().await?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response.error_for_status()?);
        }

        self.refresh_authentication(&access_token).await?;
        let access_token = self.credentials().access_token;
        Ok(build(&self.http)
            .bearer_auth(access_token)
            .send()
            .await?
            .error_for_status()?)
    }

    /// Returns the URL of one of the APIs listed by the initialization API.
    async fn resource_url(&self, name: &str) -> Result<String, KoboError> {
        let resources = self
            .resources
            .get_or_try_init(|| async {
                let url = self.endpoints.store_api.join("v1/initialization").expect("valid path");
                let initialization: Value = self
                    .send_authenticated(|http| http.get(url.clone()))
                    .await?
                    .json()
                    .await?;
                match initialization.get("Resources") {
                    Some(resources) if resources.is_object() => Ok(resources.clone()),
                    _ => Err(KoboError::UnexpectedResponse(
                        "the initialization settings have no Resources".to_owned(),
                    )),
                }
            })
            .await?;

        resources[name].as_str().map(str::to_owned).ok_or_else(|| {
            KoboError::UnexpectedResponse(format!("the initialization settings have no '{name}' resource"))
        })
    }

    /// Returns the books in the library, sorted by title. See [`parse_library`] for what is left out.
    pub async fn library(&self) -> Result<Vec<Book>, KoboError> {
        Ok(parse_library(&self.library_entitlements().await?))
    }

    /// Returns the raw entitlements from the library sync API.
    pub async fn library_entitlements(&self) -> Result<Vec<Value>, KoboError> {
        let url = self.resource_url("library_sync").await?;
        let mut entitlements = Vec::new();
        let mut sync_token: Option<String> = None;

        loop {
            let response = self
                .send_authenticated(|http| {
                    let request = http.get(&url);
                    match &sync_token {
                        Some(sync_token) => request.header("x-kobo-synctoken", sync_token),
                        None => request,
                    }
                })
                .await?;

            let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok());
            let next_sync_token = match header("x-kobo-sync") {
                Some("continue") => header("x-kobo-synctoken")
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned),
                _ => None,
            };

            let page: Vec<Value> = response.json().await?;
            entitlements.extend(page);

            match next_sync_token {
                Some(token) => sync_token = Some(token),
                None => return Ok(entitlements),
            }
        }
    }

    /// Downloads an ebook, removes the DRM, and saves it as an EPUB at `destination`.
    ///
    /// Temporary files are created next to `destination`, so the final rename can't cross filesystems. Nothing is
    /// left at `destination` if this fails. The file is created with the default permissions of temporary files,
    /// which is `0600` on Unix.
    pub async fn download_book(&self, revision_id: &str, destination: &Path) -> Result<(), KoboError> {
        let credentials = self.credentials();
        if !credentials.is_logged_in() {
            return Err(KoboError::NotLoggedIn);
        }

        let url = self
            .resource_url("content_access_book")
            .await?
            .replace("{ProductId}", revision_id);
        let content_access: Value = self
            .send_authenticated(|http| http.get(&url).query(&[("DisplayProfile", DISPLAY_PROFILE)]))
            .await?
            .json()
            .await?;
        let download = select_download(&content_access)?;
        let content_keys = content_keys(&content_access);

        let directory = match destination.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_owned(),
            _ => ".".into(),
        };
        let temporary_file = move || tempfile::Builder::new().prefix(".kobodl-").tempfile_in(&directory);

        let downloaded = temporary_file()?;
        self.download_to_file(&download.url, downloaded.path()).await?;

        let destination = destination.to_owned();
        tokio::task::spawn_blocking(move || {
            let book = if download.has_drm {
                let decrypted = temporary_file()?;
                let input = BufReader::new(File::open(downloaded.path())?);
                let mut output = DrmRemover::new(&credentials.device_id, &credentials.user_id).remove_drm(
                    input,
                    BufWriter::new(decrypted.as_file()),
                    &content_keys,
                )?;
                output.flush()?;
                drop(output);
                decrypted
            } else {
                downloaded
            };

            // A broken file is worse than none.
            check_epub(BufReader::new(File::open(book.path())?)).map_err(KoboError::CorruptBook)?;
            book.persist(&destination).map_err(|error| error.error)?;
            Ok(())
        })
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
    }

    async fn download_to_file(&self, url: &str, path: &Path) -> Result<(), KoboError> {
        // Kobo redirects downloads to requester-pays S3 URLs that are signed with this header, so send it always.
        let mut request = self.http.get(url).header("x-amz-request-payer", "requester");
        let is_kobo_host = Url::parse(url)
            .ok()
            .and_then(|url| {
                url.host_str()
                    .map(|host| host == "kobo.com" || host.ends_with(".kobo.com"))
            })
            .unwrap_or(false);
        if is_kobo_host {
            // reqwest drops this header if the request is redirected to another host.
            request = request.bearer_auth(self.credentials().access_token);
        }

        let response = request.send().await?.error_for_status()?;
        let expected = response.content_length();

        let mut file = tokio::fs::File::create(path).await?;
        let mut received = 0;
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            received += chunk.len() as u64;
            file.write_all(&chunk).await?;
        }
        file.flush().await?;

        match expected {
            Some(expected) if expected != received => Err(KoboError::IncompleteDownload { received, expected }),
            _ => Ok(()),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AuthResponse {
    token_type: String,
    access_token: String,
    refresh_token: String,
    user_key: Option<String>,
}

impl AuthResponse {
    fn check(&self) -> Result<(), KoboError> {
        if self.token_type != "Bearer" {
            return Err(KoboError::UnexpectedResponse(format!(
                "unsupported token type '{}'",
                self.token_type
            )));
        }
        if self.access_token.is_empty() || self.refresh_token.is_empty() {
            return Err(KoboError::UnexpectedResponse(
                "empty access or refresh token".to_owned(),
            ));
        }
        Ok(())
    }
}

fn client_key() -> String {
    BASE64.encode(DEFAULT_PLATFORM_ID)
}

fn random_hex(byte_count: usize) -> String {
    let mut bytes = vec![0u8; byte_count];
    rand::rng().fill(&mut bytes[..]);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_activation_page(auth_base: &Url, html: &str) -> Result<Activation, KoboError> {
    static POLL_ENDPOINT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"data-poll-endpoint="([^"]+)""#).expect("valid regex"));
    static CODE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"qrcodegenerator/generate.+?%26code%3D(\d+)").expect("valid regex"));

    let poll_endpoint = POLL_ENDPOINT.captures(html).ok_or(KoboError::ActivationPageChanged(
        "Can't find the activation poll endpoint.",
    ))?;
    let poll_url = auth_base
        .join(&html_escape::decode_html_entities(&poll_endpoint[1]))
        .map_err(|_| KoboError::ActivationPageChanged("The activation poll endpoint isn't a valid URL."))?;
    let code = CODE
        .captures(html)
        .ok_or(KoboError::ActivationPageChanged("Can't find the activation code."))?;

    Ok(Activation {
        code: code[1].to_owned(),
        poll_url: poll_url.into(),
    })
}

#[derive(Debug)]
struct Download {
    url: String,
    has_drm: bool,
}

/// Picks the download URL from a `content_access_book` response. Accepts the field names used by both Python tools.
fn select_download(content_access: &Value) -> Result<Download, KoboError> {
    let urls = content_access
        .get("ContentUrls")
        .or_else(|| content_access.get("DownloadUrls"))
        .and_then(Value::as_array)
        .ok_or_else(|| KoboError::UnexpectedResponse("the content access response has no download URLs".to_owned()))?;
    if urls.is_empty() {
        return Err(KoboError::NoDownloadUrls);
    }

    let field = |entry: &'_ Value, names: &[&str]| -> Option<String> {
        names.iter().find_map(|name| entry[name].as_str()).map(str::to_owned)
    };
    let drm_type = |entry: &Value| field(entry, &["DRMType", "DrmType"]);
    let format = |entry: &Value| field(entry, &["UrlFormat"]);

    for entry in urls {
        let has_drm = match drm_type(entry).as_deref() {
            Some("KDRM") => true,
            Some("SignedNoDrm" | "None") | None => false,
            Some(_) => continue,
        };
        if format(entry).is_some_and(|format| !SUPPORTED_FORMATS.contains(&format.as_str())) {
            continue;
        }
        if let Some(url) = field(entry, &["DownloadUrl", "Url"]) {
            return Ok(Download {
                url: remove_b_parameter(&url)?,
                has_drm,
            });
        }
    }

    if urls.iter().any(|entry| drm_type(entry).as_deref() == Some("AdobeDrm")) {
        return Err(KoboError::UnsupportedDrm("Adobe".to_owned()));
    }
    let available: Vec<String> = urls
        .iter()
        .map(|entry| {
            format!(
                "{} ({})",
                format(entry).unwrap_or_else(|| "unknown format".to_owned()),
                drm_type(entry).unwrap_or_else(|| "unknown DRM".to_owned())
            )
        })
        .collect();
    Err(KoboError::NoSupportedFormat(available.join(", ")))
}

/// Removes the "b" query parameter, which makes downloads fail with 403 Forbidden. S3 URLs are left alone, because
/// changing them would break their signature.
fn remove_b_parameter(url: &str) -> Result<String, KoboError> {
    let mut parsed =
        Url::parse(url).map_err(|_| KoboError::UnexpectedResponse(format!("invalid download URL: {url}")))?;
    if parsed.host_str().is_some_and(|host| host.contains("amazonaws.com")) {
        return Ok(url.to_owned());
    }

    let remaining: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| key != "b")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if remaining.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.query_pairs_mut().clear().extend_pairs(remaining);
    }
    Ok(parsed.into())
}

fn content_keys(content_access: &Value) -> HashMap<String, String> {
    content_access["ContentKeys"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|key| Some((key["Name"].as_str()?.to_owned(), key["Value"].as_str()?.to_owned())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_first_supported_download() {
        let content_access = json!({
            "ContentUrls": [
                { "DRMType": "AdobeDrm", "UrlFormat": "EPUB3", "DownloadUrl": "https://example.com/adobe" },
                { "DRMType": "KDRM", "UrlFormat": "PDF", "DownloadUrl": "https://example.com/pdf" },
                { "DRMType": "KDRM", "UrlFormat": "KEPUB", "DownloadUrl": "https://storedownloads.kobo.com/d?b=1&t=2" },
            ]
        });
        let download = select_download(&content_access).unwrap();
        assert_eq!(download.url, "https://storedownloads.kobo.com/d?t=2");
        assert!(download.has_drm);
    }

    #[test]
    fn accepts_kobodl_field_names() {
        let content_access = json!({
            "DownloadUrls": [{ "DrmType": "SignedNoDrm", "Url": "https://example.com/book?b=1" }]
        });
        let download = select_download(&content_access).unwrap();
        assert_eq!(download.url, "https://example.com/book");
        assert!(!download.has_drm);
    }

    #[test]
    fn reports_why_no_download_was_selected() {
        let empty = json!({ "ContentUrls": [] });
        assert!(matches!(select_download(&empty), Err(KoboError::NoDownloadUrls)));

        let adobe = json!({ "ContentUrls": [{ "DRMType": "AdobeDrm", "UrlFormat": "EPUB3", "DownloadUrl": "x" }] });
        assert!(matches!(select_download(&adobe), Err(KoboError::UnsupportedDrm(drm)) if drm == "Adobe"));

        let pdf = json!({ "ContentUrls": [{ "DRMType": "KDRM", "UrlFormat": "PDF", "DownloadUrl": "x" }] });
        assert!(matches!(select_download(&pdf), Err(KoboError::NoSupportedFormat(formats)) if formats == "PDF (KDRM)"));

        assert!(matches!(
            select_download(&json!({})),
            Err(KoboError::UnexpectedResponse(_))
        ));
    }

    #[test]
    fn keeps_s3_urls_unchanged() {
        let url = "https://bucket.s3.amazonaws.com/book?b=1&X-Amz-Signature=abc";
        assert_eq!(remove_b_parameter(url).unwrap(), url);
    }

    #[test]
    fn parses_activation_page() {
        let auth = Url::parse("https://auth.kobobooks.com/").unwrap();
        let html = r#"<div data-poll-endpoint="/ActivateOnWeb/Poll?id=1&amp;x=2"></div>
            <img src="https://www.kobo.com/qrcodegenerator/generate?text=https%3A%2F%2Fkobo.com%2Factivate%3Fx%3D1%26code%3D123456">"#;
        let activation = parse_activation_page(&auth, html).unwrap();
        assert_eq!(activation.code, "123456");
        assert_eq!(
            activation.poll_url,
            "https://auth.kobobooks.com/ActivateOnWeb/Poll?id=1&x=2"
        );

        assert!(matches!(
            parse_activation_page(&auth, "<html></html>"),
            Err(KoboError::ActivationPageChanged(_))
        ));
    }

    #[test]
    fn credentials_debug_hides_tokens() {
        let credentials = Credentials {
            access_token: "secret-access".to_owned(),
            refresh_token: "secret-refresh".to_owned(),
            user_key: "secret-key".to_owned(),
            ..Credentials::default()
        };
        assert!(!format!("{credentials:?}").contains("secret"));
    }

    #[test]
    fn random_hex_has_expected_length() {
        let id = random_hex(32);
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
