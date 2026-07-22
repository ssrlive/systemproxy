//! Get/Set system proxy. Supports Windows, macOS and linux (via gsettings).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// #[cfg(feature = "utils")]
pub mod utils;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemProxy {
    pub enable: bool,
    pub host: String,
    pub port: u16,
    pub bypass: String,
}

impl Default for SystemProxy {
    fn default() -> Self {
        Self {
            enable: false,
            host: String::new(),
            port: 0,
            #[cfg(target_os = "windows")]
            bypass: "localhost;127.*".into(),
            #[cfg(not(target_os = "windows"))]
            bypass: "localhost,127.0.0.1/8".into(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Autoproxy {
    pub enable: bool,
    pub url: String,
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to parse string `{0}`")]
    ParseStr(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("failed to get default network interface")]
    NetworkInterface,

    #[error("failed to set proxy for this environment")]
    NotSupport,

    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Xdg(#[from] xdg::BaseDirectoriesError),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl SystemProxy {
    pub fn is_support() -> bool {
        cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows",
        ))
    }

    pub fn is_enabled() -> bool {
        Self::is_support()
            && Self::get_system_proxy()
                .map(|proxy| proxy.enable)
                .unwrap_or(false)
    }

    pub fn stop() -> Result<()> {
        if !Self::is_support() {
            return Err(Error::NotSupport);
        }
        Self::set_system_proxy(&SystemProxy::default())
    }
}

impl Autoproxy {
    pub fn is_support() -> bool {
        cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows",
        ))
    }
}
