//! Turns the entitlements from the `library_sync` API into a list of books.
//!
//! Ported from `__GetBookList` and the helpers around it in kobodl's `actions.py`.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookKind {
    Ebook,
    Audiobook,
}

#[derive(Debug, Clone)]
pub struct Book {
    pub revision_id: String,
    pub title: String,
    /// The authors joined with " & ". Empty if Kobo lists no contributors.
    pub author: String,
    pub kind: BookKind,
    /// Removed from the library on the Kobo website. Kobo won't serve these until they are restored.
    pub archived: bool,
    /// Marked as finished.
    pub read: bool,
    /// The book's full metadata object, for fields this struct doesn't cover.
    pub metadata: Value,
}

impl Book {
    /// The file name the Python tools use by default: `{Author} - {Title} {ShortRevisionId}.epub`.
    pub fn file_name(&self) -> String {
        let short_revision_id: String = self.revision_id.chars().take(8).collect();
        let author = sanitize_file_name(&self.author);
        let title = sanitize_file_name(&self.title);
        format!("{author} - {title} {short_revision_id}.epub")
    }
}

/// Parses the entitlements returned by `library_sync` into books, sorted by title.
///
/// Skips previews, refunded books, subscription entitlements and anything else that isn't an ebook or audiobook.
pub fn parse_library(entitlements: &[Value]) -> Vec<Book> {
    let mut books: Vec<Book> = entitlements.iter().filter_map(parse_entitlement).collect();
    books.sort_by_cached_key(|book| book.title.to_lowercase());
    books
}

fn parse_entitlement(entitlement: &Value) -> Option<Book> {
    let new_entitlement = entitlement.get("NewEntitlement")?;

    // Skip saved previews and refunded books.
    if let Some(book_entitlement) = new_entitlement.get("BookEntitlement")
        && (book_entitlement["Accessibility"] == "Preview" || book_entitlement["IsLocked"] == true)
    {
        return None;
    }

    let (metadata, kind, archived_entitlement) = if let Some(metadata) = new_entitlement.get("BookMetadata") {
        (metadata, BookKind::Ebook, new_entitlement.get("BookEntitlement"))
    } else if let Some(metadata) = new_entitlement.get("AudiobookMetadata") {
        (
            metadata,
            BookKind::Audiobook,
            new_entitlement.get("AudiobookEntitlement"),
        )
    } else {
        if new_entitlement.get("BookSubscriptionEntitlement").is_none() {
            tracing::warn!("skipping library entry of unknown type");
        }
        return None;
    };

    let Some(revision_id) = metadata["RevisionId"].as_str() else {
        tracing::warn!("skipping library entry without a RevisionId");
        return None;
    };

    Some(Book {
        revision_id: revision_id.to_owned(),
        title: metadata["Title"].as_str().unwrap_or_default().to_owned(),
        author: author(metadata),
        kind,
        archived: archived_entitlement.is_some_and(|entitlement| entitlement["IsRemoved"] == true),
        read: new_entitlement["ReadingState"]["StatusInfo"]["Status"] == "Finished",
        metadata: metadata.clone(),
    })
}

fn author(metadata: &Value) -> String {
    let contributors = metadata["ContributorRoles"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let name = |contributor: &Value| contributor["Name"].as_str().map(str::to_owned);

    let authors: Vec<String> = contributors
        .iter()
        .filter(|contributor| contributor["Role"] == "Author")
        .filter_map(name)
        .collect();
    if !authors.is_empty() {
        return authors.join(" & ");
    }

    // The library_sync API often leaves Role empty, so fall back to the first contributor. Taking all of them would
    // include editors, translators and so on.
    contributors.first().and_then(name).unwrap_or_default()
}

fn sanitize_file_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || " ,;.!(){}[]#$'-+@_".contains(*c))
        .collect();
    // Limit the length, mostly for Windows.
    sanitized.trim_matches([' ', '.']).chars().take(100).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ebook(revision_id: &str, title: &str, book_entitlement: Value, reading_state: Value) -> Value {
        json!({
            "NewEntitlement": {
                "BookEntitlement": book_entitlement,
                "BookMetadata": {
                    "RevisionId": revision_id,
                    "Title": title,
                    "ContributorRoles": [{ "Name": "Jane Writer" }, { "Name": "Ed Itor" }],
                },
                "ReadingState": reading_state,
            }
        })
    }

    #[test]
    fn parses_and_filters_entitlements() {
        let entitlements = vec![
            ebook("rev-b", "beta", json!({ "Accessibility": "Full" }), json!(null)),
            ebook(
                "rev-a",
                "Alpha",
                json!({ "IsRemoved": true }),
                json!({ "StatusInfo": { "Status": "Finished" } }),
            ),
            ebook(
                "rev-preview",
                "Preview",
                json!({ "Accessibility": "Preview" }),
                json!(null),
            ),
            ebook("rev-refunded", "Refunded", json!({ "IsLocked": true }), json!(null)),
            json!({
                "NewEntitlement": {
                    "AudiobookEntitlement": {},
                    "AudiobookMetadata": {
                        "RevisionId": "rev-audio",
                        "Title": "Gamma",
                        "ContributorRoles": [{ "Name": "Nar Rator", "Role": "Narrator" }, { "Name": "A. Author", "Role": "Author" }],
                    },
                }
            }),
            json!({ "NewEntitlement": { "BookSubscriptionEntitlement": {} } }),
            json!({ "ChangedReadingState": {} }),
        ];

        let books = parse_library(&entitlements);

        let summary: Vec<_> = books
            .iter()
            .map(|book| {
                (
                    book.revision_id.as_str(),
                    book.kind,
                    book.archived,
                    book.read,
                    book.author.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("rev-a", BookKind::Ebook, true, true, "Jane Writer"),
                ("rev-b", BookKind::Ebook, false, false, "Jane Writer"),
                ("rev-audio", BookKind::Audiobook, false, false, "A. Author"),
            ]
        );
    }

    #[test]
    fn file_name_is_sanitized() {
        let book = Book {
            revision_id: "0123456789abcdef".to_owned(),
            title: "What/Is: This? Æøå".to_owned(),
            author: "A & B".to_owned(),
            kind: BookKind::Ebook,
            archived: false,
            read: false,
            metadata: Value::Null,
        };
        assert_eq!(book.file_name(), "A  B - WhatIs This Æøå 01234567.epub");
    }
}
