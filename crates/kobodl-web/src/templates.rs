use askama::Template;
use kobo::{Book, BookKind};

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginPage {
    pub pending: Option<PendingActivation>,
    pub error: Option<String>,
}

pub struct PendingActivation {
    pub code: String,
    pub activation_page: &'static str,
}

#[derive(Template)]
#[template(path = "library.html")]
pub struct LibraryPage {
    pub email: String,
    pub books: Vec<BookRow>,
    pub hidden_count: usize,
    pub show_read: bool,
    pub show_archived: bool,
}

pub struct BookRow {
    pub revision_id: String,
    pub title: String,
    pub author: String,
    pub read: bool,
    /// Why the book can't be downloaded, if it can't.
    pub unavailable: Option<&'static str>,
}

impl BookRow {
    pub fn new(book: &Book) -> Self {
        let unavailable = if book.kind == BookKind::Audiobook {
            Some("Audiobook, not supported")
        } else if book.archived {
            Some("Archived, restore it on kobo.com first")
        } else {
            None
        };
        Self {
            revision_id: book.revision_id.clone(),
            title: book.title.clone(),
            author: book.author.clone(),
            read: book.read,
            unavailable,
        }
    }
}

#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorPage {
    pub message: String,
}
