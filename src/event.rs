use crossterm::event::Event;

#[derive(Debug)]
pub enum AppEvent {
    /// Terminal input event (keyboard, mouse, resize)
    Input(Event),
    /// Periodic tick for UI refresh (spinners, etc.)
    Tick,
    /// Non-streaming LLM response (Phase 3). Will be replaced with streaming deltas in Phase 4.
    LlmResponse { text: String },
    /// LLM error
    LlmError { error: String },
}
