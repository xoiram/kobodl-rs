use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::body::Body;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use kobo::{ACTIVATION_PAGE, ActivationStatus, Book, BookKind, Credentials, KoboError, credentials_from_settings};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;
use tokio_util::io::ReaderStream;

use crate::AppState;
use crate::csrf::reject_cross_site_requests;
use crate::templates::{BookRow, ErrorPage, LibraryPage, LoginPage, PendingActivation};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/login", get(login_page))
        .route("/login/start", post(start_login))
        .route("/login/cancel", post(cancel_login))
        .route("/login/import", post(import_login))
        .route("/logout", post(logout))
        .route("/library", get(library_page))
        .route("/library/refresh", post(refresh_library))
        .route("/books/{revision_id}/epub", get(download_book))
        .route("/books/{revision_id}/save", post(save_book))
        .route("/healthz", get(|| async { "ok" }))
        .layer(middleware::from_fn(reject_cross_site_requests))
        .with_state(state)
}

/// Responses for the redirects after a form post. 303 makes the browser follow up with a GET.
fn see_other(location: &str) -> Response {
    Redirect::to(location).into_response()
}

async fn index(State(state): State<Arc<AppState>>) -> Response {
    if state.client().credentials().is_logged_in() {
        see_other("/library")
    } else {
        see_other("/login")
    }
}

async fn login_page(State(state): State<Arc<AppState>>) -> Result<Response, AppError> {
    let client = state.client();
    if client.credentials().is_logged_in() {
        return Ok(see_other("/library"));
    }

    let Some(activation) = state.activation().clone() else {
        return render(&LoginPage {
            pending: None,
            error: None,
        });
    };

    match client.poll_activation(&activation).await {
        Ok(ActivationStatus::Complete) => {
            tracing::info!("logged in to Kobo");
            *state.activation() = None;
            state.clear_library().await;
            Ok(see_other("/library"))
        }
        Ok(ActivationStatus::Pending) => render(&LoginPage {
            pending: Some(PendingActivation {
                code: activation.code,
                activation_page: ACTIVATION_PAGE,
            }),
            error: None,
        }),
        Err(error) => {
            tracing::warn!("activation check failed: {error}");
            *state.activation() = None;
            render(&LoginPage {
                pending: None,
                error: Some(format!("The login failed, try again. ({error})")),
            })
        }
    }
}

async fn start_login(State(state): State<Arc<AppState>>) -> Result<Response, AppError> {
    let activation = state.client().start_activation().await?;
    *state.activation() = Some(activation);
    Ok(see_other("/login"))
}

async fn cancel_login(State(state): State<Arc<AppState>>) -> Response {
    *state.activation() = None;
    see_other("/login")
}

#[derive(Deserialize)]
struct ImportForm {
    settings: String,
}

/// Logs in with the credentials from a kobo-book-downloader or kobodl settings file.
async fn import_login(State(state): State<Arc<AppState>>, Form(form): Form<ImportForm>) -> Result<Response, AppError> {
    let credentials = match credentials_from_settings(&form.settings) {
        Ok(credentials) => credentials,
        Err(error) => return login_error(format!("Can't import the login: {error}.")),
    };

    // Refresh the tokens before saving anything. That checks that Kobo still accepts them, and gives this app its own
    // tokens instead of sharing them with the other tool.
    let client = state.unsaved_client(credentials)?;
    if let Err(error) = client.refresh_tokens().await {
        tracing::warn!("importing the login failed: {error}");
        let message = match &error {
            KoboError::Http(http) if http.status().is_some_and(|status| status.is_client_error()) => {
                "Kobo didn't accept the imported login. The refresh token might have been used already or revoked. \
                 Log in with a code instead."
                    .to_owned()
            }
            _ => format!("Can't check the imported login with Kobo: {error}"),
        };
        return login_error(message);
    }

    state.replace_credentials(client.credentials())?;
    *state.activation() = None;
    state.clear_library().await;
    tracing::info!("imported Kobo login");
    Ok(see_other("/library"))
}

fn login_error(message: String) -> Result<Response, AppError> {
    let page = LoginPage {
        pending: None,
        error: Some(message),
    };
    Ok((StatusCode::UNPROCESSABLE_ENTITY, Html(page.render()?)).into_response())
}

async fn logout(State(state): State<Arc<AppState>>) -> Result<Response, AppError> {
    // Keep the device registration, so logging in again doesn't add another device to the Kobo account.
    let old = state.client().credentials();
    state.replace_credentials(Credentials {
        device_id: old.device_id,
        serial_number: old.serial_number,
        ..Credentials::default()
    })?;
    *state.activation() = None;
    state.clear_library().await;
    tracing::info!("logged out");
    Ok(see_other("/login"))
}

#[derive(Deserialize)]
struct LibraryFilters {
    #[serde(default)]
    read: bool,
    #[serde(default)]
    archived: bool,
}

impl LibraryFilters {
    fn query_string(&self) -> String {
        let mut query = Vec::new();
        if self.read {
            query.push("read=true");
        }
        if self.archived {
            query.push("archived=true");
        }
        query.join("&")
    }
}

/// The filters plus the saved-book notice. Not built with `#[serde(flatten)]`, because flattened fields lose their
/// types when parsed from a query string, and `read=true` would no longer parse as a bool.
#[derive(Deserialize)]
struct LibraryQuery {
    #[serde(default)]
    read: bool,
    #[serde(default)]
    archived: bool,
    /// The revision ID of the book that was just saved to the output folder.
    saved: Option<String>,
}

async fn library_page(
    State(state): State<Arc<AppState>>,
    Query(LibraryQuery { read, archived, saved }): Query<LibraryQuery>,
) -> Result<Response, AppError> {
    let filters = LibraryFilters { read, archived };
    let client = state.client();
    let credentials = client.credentials();
    if !credentials.is_logged_in() {
        return Ok(see_other("/login"));
    }

    let library = state.library(&client).await?;
    let books: Vec<BookRow> = library
        .iter()
        .filter(|book| (filters.read || !book.read) && (filters.archived || !book.archived))
        .map(BookRow::new)
        .collect();
    let saved_title = saved.and_then(|saved| {
        library
            .iter()
            .find(|book| book.revision_id == saved)
            .map(|book| book.title.clone())
    });

    render(&LibraryPage {
        email: credentials.email,
        hidden_count: library.len() - books.len(),
        books,
        show_read: filters.read,
        show_archived: filters.archived,
        output_dir: state.output_dir.as_ref().map(|dir| dir.display().to_string()),
        saved_title,
    })
}

async fn refresh_library(State(state): State<Arc<AppState>>) -> Response {
    state.clear_library().await;
    see_other("/library")
}

async fn download_book(
    State(state): State<Arc<AppState>>,
    Path(revision_id): Path<String>,
) -> Result<Response, AppError> {
    let client = state.client();
    let library = state.library(&client).await?;
    let book = find_ebook(&library, &revision_id)?;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("book.epub");
    client.download_book(&book.revision_id, &path).await?;
    tracing::info!(title = book.title, "downloaded book");

    let file = tokio::fs::File::open(&path).await?;
    let length = file.metadata().await?.len();
    // On Unix the open file stays readable after it's deleted, so nothing is left on disk once the response is sent.
    // On Windows the deletion fails and leaves the file in the temporary directory.
    drop(directory);

    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("application/epub+zip")),
            (header::CONTENT_LENGTH, HeaderValue::from(length)),
            (header::CONTENT_DISPOSITION, content_disposition(&book.file_name())),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}

/// Saves a book to the output folder, then goes back to the library with the same filters.
async fn save_book(
    State(state): State<Arc<AppState>>,
    Path(revision_id): Path<String>,
    Form(filters): Form<LibraryFilters>,
) -> Result<Response, AppError> {
    let output_dir = state.output_dir.as_ref().ok_or(AppError::NotFound)?;
    let client = state.client();
    let library = state.library(&client).await?;
    let book = find_ebook(&library, &revision_id)?;

    let destination = output_dir.join(book.file_name());
    client.download_book(&book.revision_id, &destination).await?;
    tracing::info!(title = book.title, path = %destination.display(), "saved book");

    let mut query = filters.query_string();
    if !query.is_empty() {
        query.push('&');
    }
    query.push_str("saved=");
    query.extend(utf8_percent_encode(&book.revision_id, NON_ALPHANUMERIC));
    Ok(see_other(&format!("/library?{query}")))
}

fn find_ebook<'a>(library: &'a [Book], revision_id: &str) -> Result<&'a Book, AppError> {
    library
        .iter()
        .find(|book| book.revision_id == revision_id && book.kind == BookKind::Ebook)
        .ok_or(AppError::NotFound)
}

/// An attachment header with an ASCII fallback name for old clients, and the real name in RFC 5987 encoding.
fn content_disposition(file_name: &str) -> HeaderValue {
    let fallback: String = file_name
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded = utf8_percent_encode(file_name, NON_ALPHANUMERIC);
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}"
    ))
    .expect("the header value only contains visible ASCII")
}

fn render(template: &impl Template) -> Result<Response, AppError> {
    Ok(Html(template.render()?).into_response())
}

enum AppError {
    Kobo(KoboError),
    NotFound,
    Internal(anyhow::Error),
}

impl From<KoboError> for AppError {
    fn from(error: KoboError) -> Self {
        Self::Kobo(error)
    }
}

impl From<anyhow::Error> for AppError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        Self::Internal(error.into())
    }
}

impl From<askama::Error> for AppError {
    fn from(error: askama::Error) -> Self {
        Self::Internal(error.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Kobo(KoboError::NotLoggedIn) => return see_other("/login"),
            Self::Kobo(KoboError::Http(error)) if error.status() == Some(StatusCode::UNAUTHORIZED) => (
                StatusCode::BAD_GATEWAY,
                "Kobo rejected the login. Log out and log in again.".to_owned(),
            ),
            Self::Kobo(error) => {
                tracing::warn!("Kobo request failed: {error}");
                (StatusCode::BAD_GATEWAY, error.to_string())
            }
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "There is no such ebook in the library.".to_owned(),
            ),
            Self::Internal(error) => {
                tracing::error!("{error:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}"))
            }
        };
        let body = ErrorPage { message }
            .render()
            .unwrap_or_else(|_| "Something went wrong.".to_owned());
        (status, Html(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_encodes_non_ascii_names() {
        let value = content_disposition("Jo Nesbø - \"Kniv\" 01234567.epub");
        assert_eq!(
            value.to_str().unwrap(),
            "attachment; filename=\"Jo Nesb_ - _Kniv_ 01234567.epub\"; \
             filename*=UTF-8''Jo%20Nesb%C3%B8%20%2D%20%22Kniv%22%2001234567%2Eepub"
        );
    }
}
