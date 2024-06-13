use std::path::PathBuf;

use async_trait::async_trait;
use reqwest_dav::{Auth, ClientBuilder};
use snafu::{ResultExt, Snafu};
use tokio_util::io::ReaderStream;

use crate::{AsyncBufReadSeek, Backend};

#[derive(Debug)]
pub struct Webdav {
    client: reqwest_dav::Client,
}

impl Webdav {
    pub async fn new(auth: Auth, url: &str) -> Result<Self, Error> {
        let client = ClientBuilder::new()
            .set_host(url.to_string())
            .set_auth(auth)
            .build()
            .context(BuildClientSnafu)?;
        client
            .list("/", reqwest_dav::Depth::Number(0))
            .await
            .context(ListFilesSnafu)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl Backend for Webdav {
    async fn upload(
        &self,
        reader: Box<dyn AsyncBufReadSeek>,
        _size: u64,
        path: PathBuf,
    ) -> Result<(), Box<dyn snafu::Error>> {
        // 删除已经存在的文件
        let _ = self.client.delete(path.to_string_lossy().as_ref()).await;

        let stream = ReaderStream::new(reader);
        let body = reqwest::Body::wrap_stream(stream);

        self.client
            .put_raw(path.to_string_lossy().as_ref(), body)
            .await
            .context(UploadSnafu)?;
        Ok(())
    }
}

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("Failed to build webdav client: {}", source))]
    BuildClient { source: reqwest_dav::Error },

    #[snafu(display("Failed to list files: {}", source))]
    ListFiles { source: reqwest_dav::Error },

    #[snafu(display("Failed to upload file: {}", source))]
    Upload { source: reqwest_dav::Error },
}
