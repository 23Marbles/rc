use std::{collections::HashMap, fs::OpenOptions, path::PathBuf, sync::LazyLock};

use argon2::PasswordHash;

use crate::daemon::CONFIG_FOLDER_PATH;

pub const USER_STORE_FILE_NAME: &str = "user-store.toml";

pub static USER_STORE_FILE_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    let cfg_folder = CONFIG_FOLDER_PATH.clone();

    cfg_folder.join(USER_STORE_FILE_NAME)
});

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct UserRecord {
    name: String,
    password_hash: String,
}

#[derive(Debug)]
pub struct UserStore {
    store: HashMap<String, PasswordHash>,
}

impl UserStore {
    pub fn read_from_file(path: PathBuf) -> anyhow::Result<Self> {
        let mut rdr = csv::Reader::from_path(path)?;

        let mut store = HashMap::new();

        for result in rdr.deserialize() {
            let UserRecord {
                name,
                password_hash,
            } = result?;

            assert!(
                store
                    .insert(name, PasswordHash::new(&password_hash)?)
                    .is_none(),
                "Cannot have multiple passwords per user"
            )
        }

        Ok(Self { store })
    }

    /// Write to the user store and overwrite previous contents
    pub fn write_to_file(&self, path: PathBuf) -> anyhow::Result<()> {
        let file = OpenOptions::new().write(true).truncate(true).open(path)?;

        let mut writer = csv::WriterBuilder::new().from_writer(file);

        for (name, hash) in self.store.iter() {
            writer.serialize(UserRecord {
                name: name.clone(),
                password_hash: hash.to_string(),
            })?;
        }

        Ok(())
    }
}
