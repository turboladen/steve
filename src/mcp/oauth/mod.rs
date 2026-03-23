//! OAuth2 authentication for remote MCP servers.

pub mod callback;
pub mod credential_store;

pub use callback::{CallbackResult, start_callback_server};
pub use credential_store::FileCredentialStore;
