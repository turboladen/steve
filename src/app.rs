use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::config::types::Config;
use crate::event::AppEvent;
use crate::project::ProjectInfo;
use crate::provider::ProviderRegistry;
use crate::storage::Storage;
use crate::ui;
use crate::ui::input::InputState;
use crate::ui::message_area::{DisplayMessage, DisplayRole, MessageAreaState};
use crate::ui::theme::Theme;

pub struct App {
    // Core state
    pub project: ProjectInfo,
    pub config: Config,
    pub storage: Storage,
    pub agents_md: Option<String>,
    pub provider_registry: Option<ProviderRegistry>,

    /// Currently selected model ref ("provider/model").
    pub current_model: Option<String>,

    // UI state
    pub input: InputState,
    pub messages: Vec<DisplayMessage>,
    pub message_area_state: MessageAreaState,
    pub theme: Theme,
    pub is_loading: bool,

    // Runtime
    event_tx: mpsc::UnboundedSender<AppEvent>,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    should_quit: bool,
}

impl App {
    pub fn new(
        project: ProjectInfo,
        config: Config,
        storage: Storage,
        agents_md: Option<String>,
        provider_registry: Option<ProviderRegistry>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Determine the default model from config
        let current_model = config.model.clone();

        Self {
            project,
            config,
            storage,
            agents_md,
            provider_registry,
            current_model,
            input: InputState::default(),
            messages: Vec::new(),
            message_area_state: MessageAreaState::default(),
            theme: Theme::default(),
            is_loading: false,
            event_tx,
            event_rx,
            should_quit: false,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut terminal = ui::setup_terminal()?;
        let mut crossterm_events = crossterm::event::EventStream::new();
        let mut tick_interval = tokio::time::interval(Duration::from_millis(100));

        // Show welcome / status
        if self.provider_registry.is_none() {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Assistant,
                text: "No providers configured. Create a steve.json config file to get started."
                    .to_string(),
            });
        }

        // Initial render
        terminal.draw(|frame| ui::render(frame, self))?;

        loop {
            tokio::select! {
                maybe_event = crossterm_events.next() => {
                    if let Some(Ok(event)) = maybe_event {
                        self.handle_event(AppEvent::Input(event)).await?;
                    }
                }
                maybe_event = self.event_rx.recv() => {
                    if let Some(event) = maybe_event {
                        self.handle_event(event).await?;
                    }
                }
                _ = tick_interval.tick() => {}
            }

            terminal.draw(|frame| ui::render(frame, self))?;

            if self.should_quit {
                break;
            }
        }

        ui::restore_terminal(&mut terminal)?;
        Ok(())
    }

    async fn handle_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::Input(Event::Key(key)) => self.handle_key(key).await?,
            AppEvent::Input(Event::Mouse(mouse)) => match mouse.kind {
                MouseEventKind::ScrollUp => self.message_area_state.scroll_up(3),
                MouseEventKind::ScrollDown => self.message_area_state.scroll_down(3),
                _ => {}
            },
            AppEvent::Input(Event::Resize(_, _)) => {}
            AppEvent::Tick => {}
            AppEvent::LlmResponse { text } => {
                self.is_loading = false;
                self.messages.push(DisplayMessage {
                    role: DisplayRole::Assistant,
                    text,
                });
                self.message_area_state.scroll_to_bottom();
            }
            AppEvent::LlmError { error } => {
                self.is_loading = false;
                self.messages.push(DisplayMessage {
                    role: DisplayRole::Assistant,
                    text: format!("[error] {error}"),
                });
                self.message_area_state.scroll_to_bottom();
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            (KeyCode::Tab, KeyModifiers::NONE) => {
                self.input.mode = self.input.mode.toggle();
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if !self.is_loading {
                    let text = self.input.take_text();
                    let trimmed = text.trim().to_string();
                    if !trimmed.is_empty() {
                        self.handle_input(trimmed).await?;
                    }
                }
            }
            _ => {
                self.input.textarea.input(key);
            }
        }
        Ok(())
    }

    async fn handle_input(&mut self, text: String) -> Result<()> {
        if text.starts_with('/') {
            return self.handle_command(&text).await;
        }

        // Add user message to display
        self.messages.push(DisplayMessage {
            role: DisplayRole::User,
            text: text.clone(),
        });
        self.message_area_state.scroll_to_bottom();

        // Try to send to LLM
        let Some(registry) = &self.provider_registry else {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Assistant,
                text: "No provider configured. Add providers to steve.json.".to_string(),
            });
            return Ok(());
        };

        let Some(model_ref) = &self.current_model else {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Assistant,
                text: "No model selected. Set 'model' in steve.json.".to_string(),
            });
            return Ok(());
        };

        let resolved = match registry.resolve_model(model_ref) {
            Ok(r) => r,
            Err(e) => {
                self.messages.push(DisplayMessage {
                    role: DisplayRole::Assistant,
                    text: format!("[error] {e}"),
                });
                return Ok(());
            }
        };

        let client = match registry.client(&resolved.provider_id) {
            Ok(c) => c.clone(),
            Err(e) => {
                self.messages.push(DisplayMessage {
                    role: DisplayRole::Assistant,
                    text: format!("[error] {e}"),
                });
                return Ok(());
            }
        };

        let model_id = resolved.api_model_id().to_string();
        let system_prompt = self.build_system_prompt();
        let event_tx = self.event_tx.clone();
        self.is_loading = true;

        // Spawn non-streaming LLM request (Phase 3: simple, blocking-style via channel)
        tokio::spawn(async move {
            match client.simple_chat(&model_id, system_prompt.as_deref(), &text).await {
                Ok(response) => {
                    let _ = event_tx.send(AppEvent::LlmResponse { text: response });
                }
                Err(e) => {
                    let _ = event_tx.send(AppEvent::LlmError {
                        error: e.to_string(),
                    });
                }
            }
        });

        Ok(())
    }

    fn build_system_prompt(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();

        parts.push(format!(
            "You are a helpful AI coding assistant. You are working in the project at: {}",
            self.project.root.display()
        ));

        if let Some(agents_md) = &self.agents_md {
            parts.push(format!("\n---\n\n{agents_md}"));
        }

        Some(parts.join("\n"))
    }

    async fn handle_command(&mut self, text: &str) -> Result<()> {
        let parts: Vec<&str> = text.splitn(2, ' ').collect();
        let cmd = parts[0];

        match cmd {
            "/exit" => {
                self.should_quit = true;
            }
            "/new" => {
                self.messages.clear();
                self.message_area_state.scroll_to_bottom();
                self.messages.push(DisplayMessage {
                    role: DisplayRole::Assistant,
                    text: "New session started.".to_string(),
                });
            }
            "/models" => {
                if let Some(registry) = &self.provider_registry {
                    let models = registry.list_models();
                    if models.is_empty() {
                        self.messages.push(DisplayMessage {
                            role: DisplayRole::Assistant,
                            text: "No models configured.".to_string(),
                        });
                    } else {
                        let list = models
                            .iter()
                            .map(|m| {
                                let current = self
                                    .current_model
                                    .as_ref()
                                    .is_some_and(|c| c == &m.display_ref());
                                let marker = if current { " (active)" } else { "" };
                                format!("  {} - {}{}", m.display_ref(), m.config.name, marker)
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        self.messages.push(DisplayMessage {
                            role: DisplayRole::Assistant,
                            text: format!("Available models:\n{list}"),
                        });
                    }
                } else {
                    self.messages.push(DisplayMessage {
                        role: DisplayRole::Assistant,
                        text: "No providers configured.".to_string(),
                    });
                }
            }
            _ => {
                self.messages.push(DisplayMessage {
                    role: DisplayRole::Assistant,
                    text: format!("Unknown command: {cmd}"),
                });
            }
        }

        Ok(())
    }
}
