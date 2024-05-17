use std::path::{Path, PathBuf};

use snafu::{ResultExt, Snafu};
use tokio::{fs::File, io::BufReader};
use tracing::debug;

use crate::Backend;

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

impl<T> Backend<BufReader<T>, Error> for Local
where
    T: tokio::io::AsyncRead + std::marker::Unpin,
{
    async fn upload<P: AsRef<Path>>(
        &self,
        reader: &mut BufReader<T>,
        _size: u64,
        path: P,
    ) -> Result<(), Error> {
        debug!("Uploading file to local: {:?}", path.as_ref());
        let path = self.folder.join(path);

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context(CreateDirSnafu)?;
        }

        // Create the file and copy the data
        let mut file = File::create(path).await.context(CreateFileSnafu)?;
        tokio::io::copy(reader, &mut file)
            .await
            .context(CopyDataSnafu)?;

        Ok(())
    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Failed to create directory: {}", source))]
    CreateDir { source: tokio::io::Error },

    #[snafu(display("Failed to create file: {}", source))]
    CreateFile { source: tokio::io::Error },

    #[snafu(display("Failed to copy data: {}", source))]
    CopyData { source: tokio::io::Error },
}

#[cfg(test)]
mod tests {

    use tokio::fs::File;
    use tokio::io::{AsyncWriteExt as _, BufReader};

    use crate::Backend as _;

    use super::Local;

    #[tokio::test]
    async fn test_upload() {
        let folder = temp_dir::TempDir::new().unwrap().path().to_path_buf();

        let info = Local::new(folder);

        // Create and write to the file
        let mut file = File::create(info.folder.join("test.txt")).await.unwrap();
        file.write_all(b"Hello, world!").await.unwrap();

        // Ensure all writes are flushed to disk
        file.sync_all().await.unwrap();

        let size = file.metadata().await.unwrap().len();

        // Reopen the file for reading
        let file = File::open(info.folder.join("test.txt")).await.unwrap();
        let mut reader = BufReader::new(file);

        let result = info.upload(&mut reader, size, "test1.txt").await;
        assert!(result.is_ok());
    }
}
