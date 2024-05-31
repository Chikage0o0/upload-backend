use std::path::PathBuf;

use async_trait::async_trait;
use snafu::{ResultExt, Snafu};
use tokio::fs::File;
use tracing::debug;

use crate::{AsyncBufReadSeek, Backend};

pub struct Local {
    folder: PathBuf,
}

impl Local {
    /// Create a new instance of the local backend.
    /// folder: The folder where the files will be stored.
    pub fn new(folder: PathBuf) -> Self {
        Self { folder }
    }
}

#[async_trait]
impl Backend for Local {
    async fn upload(
        &self,
        reader: &mut dyn AsyncBufReadSeek,
        _size: u64,
        path: PathBuf,
    ) -> Result<(), Box<dyn snafu::Error>> {
        debug!("Uploading file to local: {:?}", &path);
        let path = self.folder.join(path);

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|_| CreateDirSnafu {
                    msg: parent.to_string_lossy().to_string(),
                })?;
        }

        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .with_context(|_| CreateFileSnafu {
                    msg: path.to_string_lossy().to_string(),
                })?;
        }

        // Create the file and copy the data
        let mut file = File::create(&path)
            .await
            .with_context(|_| CreateFileSnafu {
                msg: path.to_string_lossy().to_string(),
            })?;
        tokio::io::copy(reader, &mut file)
            .await
            .with_context(|_| CopyDataSnafu {
                msg: path.to_string_lossy().to_string(),
            })?;

        Ok(())
    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Failed to create directory {}: {}", msg, source))]
    CreateDir {
        source: tokio::io::Error,
        msg: String,
    },

    #[snafu(display("Failed to create file {}: {}", msg, source))]
    CreateFile {
        source: tokio::io::Error,
        msg: String,
    },

    #[snafu(display("Failed to copy data to {}: {}", msg, source))]
    CopyData {
        source: tokio::io::Error,
        msg: String,
    },
}

#[cfg(test)]
mod tests {

    use std::fs::create_dir_all;

    use tokio::fs::File;
    use tokio::io::{AsyncWriteExt as _, BufReader};

    use super::Local;

    #[tokio::test]
    async fn test_upload() {
        let folder = temp_dir::TempDir::new().unwrap().path().to_path_buf();

        create_dir_all(&folder).unwrap();

        let local = Box::new(Local::new(folder.clone())) as Box<dyn crate::Backend>;

        // Create and write to the file
        let mut file = File::create(folder.join("test.txt")).await.unwrap();
        file.write_all(b"Hello, world!").await.unwrap();

        // Ensure all writes are flushed to disk
        file.sync_all().await.unwrap();

        let size = file.metadata().await.unwrap().len();

        // Reopen the file for reading
        let file = File::open(folder.join("test.txt")).await.unwrap();
        let mut reader = BufReader::new(file);

        let result = local.upload(&mut reader, size, "test1.txt".into()).await;
        assert!(result.is_ok());
    }
}
