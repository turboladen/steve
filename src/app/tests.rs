use super::*;

/// Create a minimal App for testing (without real storage/config).
/// Note: uses `Storage::new` which writes to the real app data dir. This is
/// acceptable because UI rendering tests don't perform storage writes. A
/// temp-dir approach would require returning `TempDir` to keep it alive.
pub(crate) fn make_test_app() -> App {
    use crate::config::types::Config;
    use crate::project::ProjectInfo;
    use crate::storage::Storage;
    use std::path::PathBuf;

    let root = PathBuf::from("/tmp/test");
    let project = ProjectInfo {
        root: root.clone(),
        id: "test".to_string(),
        cwd: root,
    };
    let config = Config::default();
    let storage = Storage::new("test-sidebar").expect("test storage");
    let usage_writer = crate::usage::test_usage_writer();
    App::new(project, config, storage, Vec::new(), None, None, Vec::new(), usage_writer)
}

/// Create a test app backed by an isolated temp directory for storage tests.
pub(super) fn make_test_app_with_storage() -> (App, tempfile::TempDir) {
    use crate::config::types::Config;
    use crate::project::ProjectInfo;
    use crate::storage::Storage;
    use std::path::PathBuf;

    let dir = tempfile::tempdir().expect("temp dir");
    let storage = Storage::with_base(dir.path().to_path_buf()).expect("storage");
    let root = PathBuf::from("/tmp/test");
    let project = ProjectInfo {
        root: root.clone(),
        id: "test-title".to_string(),
        cwd: root,
    };
    let config = Config::default();
    let usage_writer = crate::usage::test_usage_writer();
    let app = App::new(project, config, storage, Vec::new(), None, None, Vec::new(), usage_writer);
    (app, dir)
}

/// Helper: create a session via SessionManager and return the SessionInfo.
pub(super) fn create_test_session(app: &App) -> SessionInfo {
    let mgr = SessionManager::new(&app.storage, &app.project.id);
    mgr.create_session("test/model").expect("create test session")
}

/// Helper: build a ProviderRegistry with a single test model.
pub(crate) fn make_test_registry(context_window: u32) -> crate::provider::ProviderRegistry {
    use crate::config::types::{ModelCapabilities, ModelConfig, ProviderConfig};
    use std::collections::HashMap;

    let mut models = HashMap::new();
    models.insert(
        "test-model".to_string(),
        ModelConfig {
            id: "test-model".to_string(),
            name: "Test Model".to_string(),
            context_window,
            max_output_tokens: None,
            cost: None,
            capabilities: ModelCapabilities {
                tool_call: true,
                reasoning: false,
            },
        },
    );
    let provider_config = ProviderConfig {
        base_url: "https://api.test.com/v1".to_string(),
        api_key_env: "TEST_KEY".to_string(),
        models,
    };
    let client = crate::provider::client::LlmClient::new("https://api.test.com/v1", "fake");
    crate::provider::ProviderRegistry::from_entries(vec![
        ("test".to_string(), provider_config, client),
    ])
}

/// Check if any Error message contains the given substring.
pub(crate) fn has_error_message(app: &App, needle: &str) -> bool {
    app.messages.iter().any(|m| matches!(m, MessageBlock::Error { text } if text.contains(needle)))
}

/// Check if any System message contains the given substring.
pub(crate) fn has_system_message(app: &App, needle: &str) -> bool {
    app.messages.iter().any(|m| matches!(m, MessageBlock::System { text } if text.contains(needle)))
}
