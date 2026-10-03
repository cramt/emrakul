//! Where emrakul takes remote input: an EIS socket in the user's runtime
//! directory. Shared with `emrakul-portal`, which hands a connection to it
//! to whoever the RemoteDesktop portal lets in.

use std::path::PathBuf;

pub fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("emrakul-eis"))
}
