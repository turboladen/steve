//! Floating overlay for browsing MCP servers, tools, resources, and prompts.
//!
//! Follows the `diagnostics_overlay.rs` pattern: guarded by `state.visible`,
//! centered in the message area, with `Clear` behind and bordered popup.
//! Adds tab-bar navigation (Servers / Tools / Resources / Prompts).

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::theme::Theme;

// ─── Tab enum ───────────────────────────────────────────────

/// The four tabs in the MCP overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTab {
    Servers,
    Tools,
    Resources,
    Prompts,
}

impl McpTab {
    fn index(self) -> usize {
        match self {
            McpTab::Servers => 0,
            McpTab::Tools => 1,
            McpTab::Resources => 2,
            McpTab::Prompts => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            McpTab::Servers => "Servers",
            McpTab::Tools => "Tools",
            McpTab::Resources => "Resources",
            McpTab::Prompts => "Prompts",
        }
    }

    fn next(self) -> Self {
        match self {
            McpTab::Servers => McpTab::Tools,
            McpTab::Tools => McpTab::Resources,
            McpTab::Resources => McpTab::Prompts,
            McpTab::Prompts => McpTab::Servers,
        }
    }

    fn prev(self) -> Self {
        match self {
            McpTab::Servers => McpTab::Prompts,
            McpTab::Tools => McpTab::Servers,
            McpTab::Resources => McpTab::Tools,
            McpTab::Prompts => McpTab::Resources,
        }
    }

    const ALL: [McpTab; 4] = [
        McpTab::Servers,
        McpTab::Tools,
        McpTab::Resources,
        McpTab::Prompts,
    ];
}

// ─── Snapshot types ─────────────────────────────────────────

/// Owned snapshot of MCP data, taken when the overlay opens.
/// Avoids holding the async mutex during rendering.
#[derive(Debug, Clone, Default)]
pub struct McpSnapshot {
    pub servers: Vec<McpServerInfo>,
}

#[derive(Debug, Clone)]
pub struct McpServerInfo {
    pub server_id: String,
    pub connected: bool,
    pub error: Option<String>,
    pub transport: &'static str,
    pub tools: Vec<McpToolInfo>,
    pub resources: Vec<McpResourceInfo>,
    pub prompts: Vec<McpPromptInfo>,
}

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct McpResourceInfo {
    pub name: String,
    pub uri: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct McpPromptInfo {
    pub name: String,
    pub description: String,
    pub arguments: Vec<McpPromptArg>,
}

#[derive(Debug, Clone)]
pub struct McpPromptArg {
    pub name: String,
    pub description: String,
    pub required: bool,
}

// ─── Overlay state ──────────────────────────────────────────

/// State for the MCP overlay.
#[derive(Debug)]
pub struct McpOverlayState {
    /// Whether the overlay is currently visible.
    pub visible: bool,
    /// Active tab.
    active_tab: McpTab,
    /// Per-tab scroll offsets (preserved across tab switches).
    scroll_offsets: [usize; 4],
    /// Optional server_id filter (from `/mcp tools <server_id>`).
    server_filter: Option<String>,
    /// Snapshot of MCP data, taken at open time.
    snapshot: McpSnapshot,
}

impl Default for McpOverlayState {
    fn default() -> Self {
        Self {
            visible: false,
            active_tab: McpTab::Servers,
            scroll_offsets: [0; 4],
            server_filter: None,
            snapshot: McpSnapshot::default(),
        }
    }
}

impl McpOverlayState {
    /// Open the overlay on the given tab with the provided data.
    pub fn open(&mut self, tab: McpTab, snapshot: McpSnapshot, filter: Option<String>) {
        self.visible = true;
        self.active_tab = tab;
        self.scroll_offsets = [0; 4];
        self.server_filter = filter;
        self.snapshot = snapshot;
    }

    /// Close the overlay and reset state.
    pub fn close(&mut self) {
        self.visible = false;
        self.active_tab = McpTab::Servers;
        self.scroll_offsets = [0; 4];
        self.server_filter = None;
        self.snapshot = McpSnapshot::default();
    }

    /// Scroll up by `n` lines within the current tab.
    pub fn scroll_up(&mut self, n: usize) {
        let idx = self.active_tab.index();
        self.scroll_offsets[idx] = self.scroll_offsets[idx].saturating_sub(n);
    }

    /// Scroll down by `n` lines within the current tab.
    pub fn scroll_down(&mut self, n: usize) {
        let idx = self.active_tab.index();
        self.scroll_offsets[idx] = self.scroll_offsets[idx].saturating_add(n);
    }

    /// Switch to the next tab.
    pub fn next_tab(&mut self) {
        self.active_tab = self.active_tab.next();
    }

    /// Switch to the previous tab.
    pub fn prev_tab(&mut self) {
        self.active_tab = self.active_tab.prev();
    }

    /// Servers matching the current filter (or all if no filter).
    fn filtered_servers(&self) -> Vec<&McpServerInfo> {
        match &self.server_filter {
            Some(filter) => self
                .snapshot
                .servers
                .iter()
                .filter(|s| s.server_id == *filter)
                .collect(),
            None => self.snapshot.servers.iter().collect(),
        }
    }
}

// ─── Content builders ───────────────────────────────────────

fn build_tab_bar<'a>(active: McpTab, theme: &'a Theme) -> Line<'a> {
    let mut spans = Vec::new();
    spans.push(Span::raw("  "));
    for (i, tab) in McpTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default().fg(theme.dim)));
        }
        if *tab == active {
            spans.push(Span::styled(
                format!("[{}]", tab.label()),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                format!(" {} ", tab.label()),
                Style::default().fg(theme.dim),
            ));
        }
    }
    Line::from(spans)
}

fn build_servers_content<'a>(servers: &[&McpServerInfo], theme: &'a Theme) -> Vec<Line<'a>> {
    if servers.is_empty() {
        return vec![Line::from(Span::styled(
            "  No MCP servers configured",
            Style::default().fg(theme.dim),
        ))];
    }

    let mut lines: Vec<Line> = Vec::new();
    for server in servers {
        let (icon, icon_color) = if server.connected {
            ("\u{2713}", theme.success) // ✓
        } else {
            ("\u{2716}", theme.error) // ✖
        };

        // Server header: icon + name + transport
        lines.push(Line::from(vec![
            Span::styled(format!("  {icon} "), Style::default().fg(icon_color)),
            Span::styled(
                server.server_id.clone(),
                Style::default()
                    .fg(theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  [{}]", server.transport),
                Style::default().fg(theme.dim),
            ),
        ]));

        if server.connected {
            // Counts
            lines.push(Line::from(Span::styled(
                format!(
                    "    {} tools, {} resources, {} prompts",
                    server.tools.len(),
                    server.resources.len(),
                    server.prompts.len()
                ),
                Style::default().fg(theme.dim),
            )));
        } else if let Some(err) = &server.error {
            lines.push(Line::from(Span::styled(
                format!("    Error: {err}"),
                Style::default().fg(theme.error),
            )));
        }

        lines.push(Line::from(""));
    }

    // Remove trailing blank
    if lines.last().is_some_and(|l| l.spans.is_empty()) {
        lines.pop();
    }

    lines
}

fn build_tools_content<'a>(servers: &[&McpServerInfo], theme: &'a Theme) -> Vec<Line<'a>> {
    let has_tools = servers.iter().any(|s| !s.tools.is_empty());
    if !has_tools {
        return vec![Line::from(Span::styled(
            "  No tools available",
            Style::default().fg(theme.dim),
        ))];
    }

    let mut lines: Vec<Line> = Vec::new();
    for server in servers {
        if server.tools.is_empty() {
            continue;
        }
        // Server header
        lines.push(Line::from(Span::styled(
            server.server_id.clone(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));

        for tool in &server.tools {
            lines.push(Line::from(Span::styled(
                format!("  {}", tool.name),
                Style::default().fg(theme.fg),
            )));
            if !tool.description.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("    {}", tool.description),
                    Style::default().fg(theme.dim),
                )));
            }
        }
        lines.push(Line::from(""));
    }

    if lines.last().is_some_and(|l| l.spans.is_empty()) {
        lines.pop();
    }
    lines
}

fn build_resources_content<'a>(servers: &[&McpServerInfo], theme: &'a Theme) -> Vec<Line<'a>> {
    let has_resources = servers.iter().any(|s| !s.resources.is_empty());
    if !has_resources {
        return vec![Line::from(Span::styled(
            "  No resources available",
            Style::default().fg(theme.dim),
        ))];
    }

    let mut lines: Vec<Line> = Vec::new();
    for server in servers {
        if server.resources.is_empty() {
            continue;
        }
        lines.push(Line::from(Span::styled(
            server.server_id.clone(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));

        for res in &server.resources {
            lines.push(Line::from(vec![
                Span::styled(format!("  {}", res.name), Style::default().fg(theme.fg)),
                Span::styled(format!("  ({})", res.uri), Style::default().fg(theme.dim)),
            ]));
            if !res.description.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("    {}", res.description),
                    Style::default().fg(theme.dim),
                )));
            }
        }
        lines.push(Line::from(""));
    }

    if lines.last().is_some_and(|l| l.spans.is_empty()) {
        lines.pop();
    }
    lines
}

fn build_prompts_content<'a>(servers: &[&McpServerInfo], theme: &'a Theme) -> Vec<Line<'a>> {
    let has_prompts = servers.iter().any(|s| !s.prompts.is_empty());
    if !has_prompts {
        return vec![Line::from(Span::styled(
            "  No prompts available",
            Style::default().fg(theme.dim),
        ))];
    }

    let mut lines: Vec<Line> = Vec::new();
    for server in servers {
        if server.prompts.is_empty() {
            continue;
        }
        lines.push(Line::from(Span::styled(
            server.server_id.clone(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )));

        for prompt in &server.prompts {
            lines.push(Line::from(Span::styled(
                format!("  {}", prompt.name),
                Style::default().fg(theme.fg),
            )));
            if !prompt.description.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("    {}", prompt.description),
                    Style::default().fg(theme.dim),
                )));
            }
            if !prompt.arguments.is_empty() {
                let args_str: Vec<String> = prompt
                    .arguments
                    .iter()
                    .map(|a| {
                        if a.required {
                            a.name.clone()
                        } else {
                            format!("{}?", a.name)
                        }
                    })
                    .collect();
                lines.push(Line::from(Span::styled(
                    format!("    args: {}", args_str.join(", ")),
                    Style::default().fg(theme.dim),
                )));
            }
        }
        lines.push(Line::from(""));
    }

    if lines.last().is_some_and(|l| l.spans.is_empty()) {
        lines.pop();
    }
    lines
}

// ─── Render ─────────────────────────────────────────────────

/// Render the MCP overlay centered in the message area.
pub fn render_mcp_overlay(
    frame: &mut Frame,
    message_area: Rect,
    state: &McpOverlayState,
    theme: &Theme,
    context_pct: u8,
) {
    if !state.visible {
        return;
    }

    let servers = state.filtered_servers();

    // Build content for active tab
    let content_lines = match state.active_tab {
        McpTab::Servers => build_servers_content(&servers, theme),
        McpTab::Tools => build_tools_content(&servers, theme),
        McpTab::Resources => build_resources_content(&servers, theme),
        McpTab::Prompts => build_prompts_content(&servers, theme),
    };

    // Tab bar + blank + content
    let tab_bar = build_tab_bar(state.active_tab, theme);
    let mut lines = vec![tab_bar, Line::from("")];
    lines.extend(content_lines);

    // Calculate popup dimensions
    let content_height = lines.len() as u16;
    let popup_height = (content_height + 4) // +2 border +2 padding
        .min(message_area.height.saturating_sub(2))
        .max(10);
    let popup_width = 70u16.min(message_area.width.saturating_sub(4));

    if popup_width < 20 || popup_height < 5 {
        return;
    }

    // Center in message area
    let popup_x = message_area.x + (message_area.width.saturating_sub(popup_width)) / 2;
    let popup_y = message_area.y + (message_area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    // Clear behind the popup
    frame.render_widget(Clear, popup_area);

    // Title — contextual to filter
    let title_text = match &state.server_filter {
        Some(id) => format!(" MCP \u{2014} {id} "),
        None => " MCP ".to_string(),
    };

    let border_style = Style::default().fg(theme.border_color(context_pct));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![Span::styled(
            title_text,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )]))
        .title_bottom(Line::from(vec![Span::styled(
            " Tab switch  \u{2191}\u{2193} scroll  Esc close ",
            Style::default().fg(theme.dim),
        )]));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Render content with scroll
    let scroll_offset = state.scroll_offsets[state.active_tab.index()];
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset as u16, 0));

    frame.render_widget(paragraph, inner);
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn sample_snapshot() -> McpSnapshot {
        McpSnapshot {
            servers: vec![
                McpServerInfo {
                    server_id: "github".to_string(),
                    connected: true,
                    error: None,
                    transport: "http",
                    tools: vec![
                        McpToolInfo {
                            name: "search_repos".to_string(),
                            description: "Search GitHub repositories".to_string(),
                        },
                        McpToolInfo {
                            name: "get_issue".to_string(),
                            description: "Get issue details".to_string(),
                        },
                    ],
                    resources: vec![McpResourceInfo {
                        name: "repo".to_string(),
                        uri: "github://repo/owner/name".to_string(),
                        description: "Repository content".to_string(),
                    }],
                    prompts: vec![McpPromptInfo {
                        name: "summarize_pr".to_string(),
                        description: "Summarize a pull request".to_string(),
                        arguments: vec![McpPromptArg {
                            name: "pr_url".to_string(),
                            description: "PR URL".to_string(),
                            required: true,
                        }],
                    }],
                },
                McpServerInfo {
                    server_id: "filesystem".to_string(),
                    connected: false,
                    error: Some("Connection refused".to_string()),
                    transport: "stdio",
                    tools: vec![],
                    resources: vec![],
                    prompts: vec![],
                },
            ],
        }
    }

    // ─── State tests ───

    #[test]
    fn default_not_visible() {
        let state = McpOverlayState::default();
        assert!(!state.visible);
        assert_eq!(state.active_tab, McpTab::Servers);
        assert_eq!(state.scroll_offsets, [0; 4]);
        assert!(state.server_filter.is_none());
    }

    #[test]
    fn open_sets_tab_and_visible() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Tools, sample_snapshot(), None);
        assert!(state.visible);
        assert_eq!(state.active_tab, McpTab::Tools);
        assert_eq!(state.snapshot.servers.len(), 2);
    }

    #[test]
    fn open_with_filter() {
        let mut state = McpOverlayState::default();
        state.open(
            McpTab::Resources,
            sample_snapshot(),
            Some("github".to_string()),
        );
        assert_eq!(state.server_filter, Some("github".to_string()));
        assert_eq!(state.filtered_servers().len(), 1);
        assert_eq!(state.filtered_servers()[0].server_id, "github");
    }

    #[test]
    fn close_resets_all() {
        let mut state = McpOverlayState::default();
        state.open(
            McpTab::Prompts,
            sample_snapshot(),
            Some("github".to_string()),
        );
        state.scroll_down(5);
        state.close();
        assert!(!state.visible);
        assert_eq!(state.active_tab, McpTab::Servers);
        assert_eq!(state.scroll_offsets, [0; 4]);
        assert!(state.server_filter.is_none());
        assert!(state.snapshot.servers.is_empty());
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Servers, sample_snapshot(), None);
        state.scroll_up(10);
        assert_eq!(state.scroll_offsets[McpTab::Servers.index()], 0);
    }

    #[test]
    fn scroll_down_increments() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Servers, sample_snapshot(), None);
        state.scroll_down(3);
        assert_eq!(state.scroll_offsets[McpTab::Servers.index()], 3);
    }

    #[test]
    fn tab_cycling_wraps() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Prompts, sample_snapshot(), None);
        state.next_tab();
        assert_eq!(state.active_tab, McpTab::Servers);

        state.prev_tab();
        assert_eq!(state.active_tab, McpTab::Prompts);
    }

    #[test]
    fn per_tab_scroll_preserved() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Servers, sample_snapshot(), None);
        state.scroll_down(5);
        assert_eq!(state.scroll_offsets[McpTab::Servers.index()], 5);

        state.next_tab(); // Tools
        state.scroll_down(3);
        assert_eq!(state.scroll_offsets[McpTab::Tools.index()], 3);

        state.prev_tab(); // Back to Servers
        assert_eq!(state.scroll_offsets[McpTab::Servers.index()], 5);
    }

    #[test]
    fn filter_no_match_returns_empty() {
        let mut state = McpOverlayState::default();
        state.open(
            McpTab::Servers,
            sample_snapshot(),
            Some("nonexistent".to_string()),
        );
        assert!(state.filtered_servers().is_empty());
    }

    // ─── Render tests ───

    fn render_overlay_to_string(
        width: u16,
        height: u16,
        state: &McpOverlayState,
        message_area: Rect,
    ) -> String {
        let theme = Theme::default();
        let buf = super::super::render_to_buffer(width, height, |frame| {
            render_mcp_overlay(frame, message_area, state, &theme, 0);
        });
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = &buf[(x, y)];
                text.push_str(cell.symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn render_hidden_shows_nothing() {
        let state = McpOverlayState::default();
        let area = Rect::new(0, 0, 80, 24);
        let text = render_overlay_to_string(80, 24, &state, area);
        let non_space: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            non_space.is_empty(),
            "hidden overlay should render nothing, got: '{non_space}'"
        );
    }

    #[test]
    fn render_shows_title() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Servers, sample_snapshot(), None);
        let area = Rect::new(0, 0, 80, 24);
        let text = render_overlay_to_string(80, 24, &state, area);
        assert!(text.contains("MCP"), "should show MCP title, got:\n{text}");
    }

    #[test]
    fn render_shows_tab_bar() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Servers, sample_snapshot(), None);
        let area = Rect::new(0, 0, 80, 24);
        let text = render_overlay_to_string(80, 24, &state, area);
        assert!(text.contains("Servers"), "tab bar should show Servers");
        assert!(text.contains("Tools"), "tab bar should show Tools");
        assert!(text.contains("Resources"), "tab bar should show Resources");
        assert!(text.contains("Prompts"), "tab bar should show Prompts");
    }

    #[test]
    fn render_shows_server_info() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Servers, sample_snapshot(), None);
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("github"), "should show github server");
        assert!(text.contains("filesystem"), "should show filesystem server");
        assert!(
            text.contains("Connection refused"),
            "should show error for failed server"
        );
    }

    #[test]
    fn render_shows_tool_names() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Tools, sample_snapshot(), None);
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("search_repos"), "should show tool name");
        assert!(text.contains("get_issue"), "should show tool name");
    }

    #[test]
    fn render_shows_resources() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Resources, sample_snapshot(), None);
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("repo"), "should show resource name");
        assert!(
            text.contains("github://repo/owner/name"),
            "should show resource URI"
        );
    }

    #[test]
    fn render_shows_prompts() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Prompts, sample_snapshot(), None);
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("summarize_pr"), "should show prompt name");
        assert!(text.contains("pr_url"), "should show prompt arg");
    }

    #[test]
    fn render_empty_shows_message() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Servers, McpSnapshot::default(), None);
        let area = Rect::new(0, 0, 80, 24);
        let text = render_overlay_to_string(80, 24, &state, area);
        assert!(
            text.contains("No MCP servers"),
            "should show empty message, got:\n{text}"
        );
    }

    #[test]
    fn render_shows_hints() {
        let mut state = McpOverlayState::default();
        state.open(McpTab::Servers, sample_snapshot(), None);
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        assert!(text.contains("Esc close"), "should show key hints");
    }

    #[test]
    fn render_filtered_title_shows_server_id() {
        let mut state = McpOverlayState::default();
        state.open(
            McpTab::Tools,
            sample_snapshot(),
            Some("github".to_string()),
        );
        let area = Rect::new(0, 0, 80, 30);
        let text = render_overlay_to_string(80, 30, &state, area);
        // Title should include the filter server id (with em-dash separator)
        assert!(
            text.contains("github"),
            "filtered title should show server id"
        );
    }
}
