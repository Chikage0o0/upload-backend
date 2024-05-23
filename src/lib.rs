use std::path::PathBuf;

use async_trait::async_trait;

pub mod backend;

pub trait AsyncBufReadSeek:
    tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin + Send + Sync
{
}

impl<T> AsyncBufReadSeek for T where
    T: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin + Send + Sync
{
}

#[async_trait]
pub trait Backend {
    async fn upload(
        &self,
        reader: &mut dyn AsyncBufReadSeek,
        size: u64,
        path: PathBuf,
    ) -> Result<(), Box<dyn snafu::Error>>;
}
