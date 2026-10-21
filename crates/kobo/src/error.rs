use crate::drm::DrmError;

#[derive(Debug, thiserror::Error)]
pub enum KoboError {
    #[error("request to Kobo failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("not logged in to Kobo")]
    NotLoggedIn,

    #[error("unexpected response from Kobo: {0}")]
    UnexpectedResponse(String),

    #[error("{0} The Kobo activation page might have changed.")]
    ActivationPageChanged(&'static str),

    #[error(
        "Kobo returned no download URLs for this book. Archived books have to be restored on the Kobo website before \
         they can be downloaded."
    )]
    NoDownloadUrls,

    #[error("the book is protected with {0} DRM, which isn't supported")]
    UnsupportedDrm(String),

    #[error("Kobo offers no supported format for this book. Available formats: {0}")]
    NoSupportedFormat(String),

    #[error("the download is incomplete: got {received} bytes, expected {expected}")]
    IncompleteDownload { received: u64, expected: u64 },

    #[error("the downloaded book is corrupt: {0}")]
    CorruptBook(String),

    #[error(transparent)]
    Drm(#[from] DrmError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
