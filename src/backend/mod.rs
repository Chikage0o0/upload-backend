mod local;

pub use local::Local;

#[cfg(feature = "onedrive")]
mod onedrive;
#[cfg(feature = "onedrive")]
pub use onedrive::ApiType as OnedriveApiType;
#[cfg(feature = "onedrive")]
pub use onedrive::Onedrive;

#[cfg(feature = "webdav")]
mod webdav;
#[cfg(feature = "webdav")]
pub use reqwest_dav::Auth;
#[cfg(feature = "webdav")]
pub use webdav::Webdav;
