//! Web UI for downloading Kobo books as DRM-free EPUBs.

mod csrf;
mod routes;
mod store;
mod templates;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use axum::Router;
use kobo::{Activation, Book, Credentials, Endpoints, KoboClient, KoboError};

pub use store::CredentialsStore;

pub struct Config {
    /// Where `state.json` is kept.
    pub data_dir: PathBuf,
    /// The Kobo service URLs. Only tests change these.
    pub endpoints: Endpoints,
}

pub fn app(config: Config) -> anyhow::Result<Router> {
    let store = CredentialsStore::new(&config.data_dir);
    let client = new_client(&store, &config.endpoints, store.load()?)?;
    let state = AppState {
        store,
        endpoints: config.endpoints,
        client: RwLock::new(Arc::new(client)),
        activation: Mutex::new(None),
        library: tokio::sync::Mutex::new(None),
    };
    Ok(routes::router(Arc::new(state)))
}

struct AppState {
    store: CredentialsStore,
    endpoints: Endpoints,
    /// Replaced on logout, so that requests already in flight finish with the client they started with.
    client: RwLock<Arc<KoboClient>>,
    /// The login waiting for the user to enter the code on the Kobo website.
    activation: Mutex<Option<Activation>>,
    /// The library from the last sync. It's only fetched again when the user asks for it.
    library: tokio::sync::Mutex<Option<Arc<Vec<Book>>>>,
}

impl AppState {
    fn client(&self) -> Arc<KoboClient> {
        self.client.read().expect("client lock poisoned").clone()
    }

    fn activation(&self) -> std::sync::MutexGuard<'_, Option<Activation>> {
        self.activation.lock().expect("activation lock poisoned")
    }

    /// Saves the credentials and switches to a client that uses them.
    fn replace_credentials(&self, credentials: Credentials) -> anyhow::Result<()> {
        self.store.save(&credentials)?;
        let client = new_client(&self.store, &self.endpoints, credentials)?;
        *self.client.write().expect("client lock poisoned") = Arc::new(client);
        Ok(())
    }

    async fn library(&self, client: &KoboClient) -> Result<Arc<Vec<Book>>, KoboError> {
        let mut cached = self.library.lock().await;
        if let Some(books) = cached.as_ref() {
            return Ok(books.clone());
        }
        let books = Arc::new(client.library().await?);
        *cached = Some(books.clone());
        Ok(books)
    }

    async fn clear_library(&self) {
        *self.library.lock().await = None;
    }
}

fn new_client(store: &CredentialsStore, endpoints: &Endpoints, credentials: Credentials) -> anyhow::Result<KoboClient> {
    let store = store.clone();
    Ok(KoboClient::new(credentials)?
        .with_endpoints(endpoints.clone())
        .on_credentials_changed(move |credentials| {
            if let Err(error) = store.save(credentials) {
                tracing::error!("can't save the Kobo credentials: {error:#}");
            }
        }))
}
