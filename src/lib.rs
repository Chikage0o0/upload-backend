use std::{future::Future, path::Path};

use tokio::io::AsyncBufRead;

pub mod backend;

pub trait Backend<R: AsyncBufRead + Sized, E: snafu::Error + Sized> {
    fn upload<P: AsRef<Path>>(
        &self,
        reader: &mut R,
        size: u64,
        path: P,
    ) -> impl Future<Output = Result<(), E>>;
}
