//! The search root for the directory picker (shared vocabulary; the
//! kernel re-exports under `jinn_domain::protocol::CwdRoot`).

/// The search root for the directory picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwdRoot {
    /// Search from the active session's current CWD.
    Session,
    /// Search from the user's home directory.
    Home,
}

impl std::fmt::Display for CwdRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CwdRoot::Session => write!(f, "session"),
            CwdRoot::Home => write!(f, "home"),
        }
    }
}
