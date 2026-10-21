//! Kobo store API client and DRM removal.

pub mod client;
pub mod drm;
mod epub;
mod error;
pub mod import;
pub mod library;

pub use client::{ACTIVATION_PAGE, Activation, ActivationStatus, Credentials, Endpoints, KoboClient};
pub use drm::{DrmError, DrmRemover};
pub use epub::check_epub;
pub use error::KoboError;
pub use import::{ImportError, credentials_from_settings};
pub use library::{Book, BookKind};
