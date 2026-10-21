//! Persists the Kobo credentials in `state.json` in the data directory.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use kobo::Credentials;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct StoredState {
    #[serde(default)]
    credentials: Credentials,
}

#[derive(Clone)]
pub struct CredentialsStore {
    path: PathBuf,
}

impl CredentialsStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("state.json"),
        }
    }

    /// Returns empty credentials if there is no state file yet.
    pub fn load(&self) -> anyhow::Result<Credentials> {
        match std::fs::read_to_string(&self.path) {
            Ok(json) => {
                let state: StoredState =
                    serde_json::from_str(&json).with_context(|| format!("can't parse {}", self.path.display()))?;
                Ok(state.credentials)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Credentials::default()),
            Err(error) => Err(error).with_context(|| format!("can't read {}", self.path.display())),
        }
    }

    /// Writes the file atomically. It is only readable by the owner, because it holds the account tokens.
    pub fn save(&self, credentials: &Credentials) -> anyhow::Result<()> {
        let directory = self.path.parent().unwrap_or(Path::new("."));
        // Temporary files are created with mode 0600 on Unix, and the rename keeps that.
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(
            &mut file,
            &StoredState {
                credentials: credentials.clone(),
            },
        )?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(&self.path)
            .with_context(|| format!("can't write {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialsStore::new(directory.path());
        assert_eq!(store.load().unwrap(), Credentials::default());

        let credentials = Credentials {
            device_id: "device".to_owned(),
            access_token: "token".to_owned(),
            ..Credentials::default()
        };
        store.save(&credentials).unwrap();
        assert_eq!(store.load().unwrap(), credentials);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(directory.path().join("state.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
