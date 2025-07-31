use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::*,
};
use serde_json::Value;
use sos::http::{CreatePayload, ExecPayload, SandboxInfo, StopPayload};

#[derive(Debug, Clone)]
enum AppScreen {
    SandboxList,
    SandboxDetail(String), // sandbox ID
    NewSandbox,
    SandboxSession(String), // sandbox ID
}

#[derive(Debug, Clone)]
struct SandboxDetailState {
    trajectory: String,
    formatted: bool,
    scroll_offset: usize,
}

#[derive(Debug, Clone)]
struct NewSandboxState {
    image: String,
    setup_commands: Vec<String>,
    current_command: String,
    step: NewSandboxStep,
    session_active: bool,
    sandbox_id: Option<String>,
}

#[derive(Debug, Clone)]
enum NewSandboxStep {
    EnterImage,
    EnterSetupCommands,
    Creating,
    SessionReady,
}

#[derive(Debug, Clone)]
struct SessionState {
    history: Vec<String>,
    current_input: String,
    scroll_offset: usize,
}

struct App {
    should_quit: bool,
    current_screen: AppScreen,
    sandbox_list: Vec<SandboxInfo>,
    selected_sandbox: usize,
    list_scroll_offset: usize,
    detail_state: SandboxDetailState,
    new_sandbox_state: NewSandboxState,
    session_state: SessionState,
    server_url: String,
    client: reqwest::Client,
    status_message: Option<String>,
    input_mode: bool,
    vim_command_buffer: String,
}

impl App {
    fn new(server_url: String) -> Self {
        Self {
            should_quit: false,
            current_screen: AppScreen::SandboxList,
            sandbox_list: Vec::new(),
            selected_sandbox: 0,
            list_scroll_offset: 0,
            detail_state: SandboxDetailState {
                trajectory: String::new(),
                formatted: true,
                scroll_offset: 0,
            },
            new_sandbox_state: NewSandboxState {
                image: "ubuntu:latest".to_string(),
                setup_commands: Vec::new(),
                current_command: String::new(),
                step: NewSandboxStep::EnterImage,
                session_active: false,
                sandbox_id: None,
            },
            session_state: SessionState {
                history: Vec::new(),
                current_input: String::new(),
                scroll_offset: 0,
            },
            server_url,
            client: reqwest::Client::new(),
            status_message: None,
            input_mode: false,
            vim_command_buffer: String::new(),
        }
    }

    async fn refresh_sandbox_list(&mut self) -> Result<()> {
        let response = self
            .client
            .get(&format!("{}/sandboxes", self.server_url))
            .send()
            .await?;

        if response.status().is_success() {
            self.sandbox_list = response.json().await?;
            if self.selected_sandbox >= self.sandbox_list.len() && !self.sandbox_list.is_empty() {
                self.selected_sandbox = self.sandbox_list.len() - 1;
            }
            self.update_list_scroll();
        } else {
            self.status_message = Some(format!("Failed to refresh: {}", response.text().await?));
        }
        Ok(())
    }

    fn update_list_scroll(&mut self) {
        // This will be called with viewport height when drawing
        // For now, just ensure we don't scroll past bounds
        if self.list_scroll_offset > self.selected_sandbox {
            self.list_scroll_offset = self.selected_sandbox;
        }
    }

    fn update_list_scroll_with_viewport(&mut self, viewport_height: usize) {
        let viewport_height = viewport_height.saturating_sub(2); // Account for borders
        
        if viewport_height == 0 {
            return;
        }

        // If selected item is above the viewport, scroll up
        if self.selected_sandbox < self.list_scroll_offset {
            self.list_scroll_offset = self.selected_sandbox;
        }
        // If selected item is below the viewport, scroll down
        else if self.selected_sandbox >= self.list_scroll_offset + viewport_height {
            self.list_scroll_offset = self.selected_sandbox.saturating_sub(viewport_height - 1);
        }
    }

    fn goto_first_sandbox(&mut self) {
        self.selected_sandbox = 0;
        self.list_scroll_offset = 0;
    }

    fn goto_last_sandbox(&mut self) {
        if !self.sandbox_list.is_empty() {
            self.selected_sandbox = self.sandbox_list.len() - 1;
            // Scroll will be updated in the drawing function
        }
    }

    async fn load_trajectory(&mut self, sandbox_id: &str) -> Result<()> {
        let endpoint = if self.detail_state.formatted {
            format!("{}/sandboxes/{}/trajectory/formatted", self.server_url, sandbox_id)
        } else {
            format!("{}/sandboxes/{}/trajectory", self.server_url, sandbox_id)
        };

        let response = self.client.get(&endpoint).send().await?;

        if response.status().is_success() {
            if self.detail_state.formatted {
                self.detail_state.trajectory = response.text().await?;
            } else {
                let json: Value = response.json().await?;
                self.detail_state.trajectory = serde_json::to_string_pretty(&json)?;
            }
        } else {
            self.detail_state.trajectory = format!("Failed to load trajectory: {}", response.text().await?);
        }
        Ok(())
    }

    async fn create_sandbox(&mut self) -> Result<()> {
        let payload = CreatePayload {
            image: self.new_sandbox_state.image.clone(),
            setup_commands: self.new_sandbox_state.setup_commands.clone(),
        };

        let response = self
            .client
            .post(&format!("{}/sandboxes", self.server_url))
            .json(&payload)
            .send()
            .await?;

        if response.status().is_success() {
            let result: Value = response.json().await?;
            let id = result["id"].as_str().unwrap().to_string();
            self.new_sandbox_state.sandbox_id = Some(id.clone());
            self.status_message = Some(format!("Sandbox created: {}", id));
            
            // Start the sandbox
            let start_response = self
                .client
                .post(&format!("{}/sandboxes/{}/start", self.server_url, id))
                .send()
                .await?;
                
            if start_response.status().is_success() {
                self.new_sandbox_state.step = NewSandboxStep::SessionReady;
                self.new_sandbox_state.session_active = true;
                self.session_state.history.clear();
                self.session_state.history.push(format!("Sandbox {} started successfully", id));
                self.input_mode = true; // Enable input mode for session
            } else {
                self.status_message = Some(format!("Failed to start sandbox: {}", start_response.text().await?));
            }
        } else {
            self.status_message = Some(format!("Failed to create sandbox: {}", response.text().await?));
        }
        Ok(())
    }

    async fn execute_command(&mut self, command: &str, sandbox_id: &str) -> Result<()> {
        let payload = ExecPayload {
            command: command.to_string(),
            standalone: None,
        };

        let response = self
            .client
            .post(&format!("{}/sandboxes/{}/exec", self.server_url, sandbox_id))
            .json(&payload)
            .send()
            .await?;

        if response.status().is_success() {
            let result: Value = response.json().await?;
            let output = result["output"].as_str().unwrap_or("");
            let exit_code = result["exit_code"].as_i64().unwrap_or(-4);

            self.session_state.history.push(format!("$ {}", command));
            if !output.is_empty() {
                for line in output.lines() {
                    self.session_state.history.push(line.to_string());
                }
            }
            if exit_code != 0 {
                self.session_state.history.push(format!("(exit code: {})", exit_code));
            }
        } else {
            self.session_state.history.push(format!("Failed to execute: {}", response.text().await?));
        }
        Ok(())
    }

    async fn stop_sandbox(&mut self, sandbox_id: &str, remove: bool) -> Result<()> {
        let payload = StopPayload { remove: Some(remove) };
        
        let response = self
            .client
            .post(&format!("{}/sandboxes/{}/stop", self.server_url, sandbox_id))
            .json(&payload)
            .send()
            .await?;

        if response.status().is_success() {
            self.status_message = Some(format!("Sandbox {} stopped", sandbox_id));
        } else {
            self.status_message = Some(format!("Failed to stop sandbox: {}", response.text().await?));
        }
        Ok(())
    }

    async fn handle_key_event(&mut self, key: event::KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        match self.current_screen.clone() {
            AppScreen::SandboxList => {
                match key.code {
                    KeyCode::Char('q') => self.should_quit = true,
                    KeyCode::Char('r') => {
                        self.refresh_sandbox_list().await?;
                    }
                    KeyCode::Char('n') => {
                        self.current_screen = AppScreen::NewSandbox;
                        self.new_sandbox_state = NewSandboxState {
                            image: "ubuntu:latest".to_string(),
                            setup_commands: Vec::new(),
                            current_command: String::new(),
                            step: NewSandboxStep::EnterImage,
                            session_active: false,
                            sandbox_id: None,
                        };
                        self.input_mode = true;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if self.selected_sandbox > 0 {
                            self.selected_sandbox -= 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if self.selected_sandbox < self.sandbox_list.len().saturating_sub(1) {
                            self.selected_sandbox += 1;
                        }
                    }
                    KeyCode::Char('g') => {
                        self.vim_command_buffer.push('g');
                        if self.vim_command_buffer == "gg" {
                            self.goto_first_sandbox();
                            self.vim_command_buffer.clear();
                        }
                    }
                    KeyCode::Char('G') => {
                        self.goto_last_sandbox();
                        self.vim_command_buffer.clear();
                    }
                    KeyCode::Enter => {
                        if !self.sandbox_list.is_empty() {
                            let sandbox_id = self.sandbox_list[self.selected_sandbox].id.clone();
                            self.current_screen = AppScreen::SandboxDetail(sandbox_id.clone());
                            self.load_trajectory(&sandbox_id).await?;
                        }
                    }
                    _ => {
                        // Clear vim command buffer on any other key
                        self.vim_command_buffer.clear();
                    }
                }
            }
            AppScreen::SandboxDetail(sandbox_id) => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.current_screen = AppScreen::SandboxList;
                        self.refresh_sandbox_list().await?;
                    }
                    KeyCode::Char('t') => {
                        self.detail_state.formatted = !self.detail_state.formatted;
                        self.load_trajectory(&sandbox_id).await?;
                    }
                    KeyCode::Char('s') => {
                        self.current_screen = AppScreen::SandboxSession(sandbox_id.clone());
                        self.session_state.history.clear();
                        self.session_state.current_input.clear();
                        self.input_mode = true;
                    }
                    KeyCode::Char('x') => {
                        self.stop_sandbox(&sandbox_id, true).await?;
                        self.current_screen = AppScreen::SandboxList;
                        self.refresh_sandbox_list().await?;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.detail_state.scroll_offset = self.detail_state.scroll_offset.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.detail_state.scroll_offset += 1;
                    }
                    _ => {}
                }
            }
            AppScreen::NewSandbox => {
                if self.input_mode {
                    match &self.new_sandbox_state.step {
                        NewSandboxStep::EnterImage => {
                            match key.code {
                                KeyCode::Enter => {
                                    self.new_sandbox_state.step = NewSandboxStep::EnterSetupCommands;
                                }
                                KeyCode::Char(c) => {
                                    self.new_sandbox_state.image.push(c);
                                }
                                KeyCode::Backspace => {
                                    self.new_sandbox_state.image.pop();
                                }
                                KeyCode::Esc => {
                                    self.current_screen = AppScreen::SandboxList;
                                    self.input_mode = false;
                                }
                                _ => {}
                            }
                        }
                        NewSandboxStep::EnterSetupCommands => {
                            match key.code {
                                KeyCode::Enter => {
                                    if !self.new_sandbox_state.current_command.is_empty() {
                                        self.new_sandbox_state.setup_commands.push(self.new_sandbox_state.current_command.clone());
                                        self.new_sandbox_state.current_command.clear();
                                    } else {
                                        self.new_sandbox_state.step = NewSandboxStep::Creating;
                                        self.input_mode = false;
                                        let _ = self.create_sandbox().await;
                                    }
                                }
                                KeyCode::Char(c) => {
                                    self.new_sandbox_state.current_command.push(c);
                                }
                                KeyCode::Backspace => {
                                    self.new_sandbox_state.current_command.pop();
                                }
                                KeyCode::Esc => {
                                    self.current_screen = AppScreen::SandboxList;
                                    self.input_mode = false;
                                }
                                _ => {}
                            }
                        }
                        NewSandboxStep::SessionReady => {
                            match key.code {
                                KeyCode::Enter => {
                                    if !self.session_state.current_input.is_empty() {
                                        let command = self.session_state.current_input.clone();
                                        let sandbox_id = self.new_sandbox_state.sandbox_id.clone();
                                        self.session_state.current_input.clear();
                                        if let Some(sandbox_id) = sandbox_id {
                                            self.execute_command(&command, &sandbox_id).await?;
                                        }
                                    }
                                }
                                KeyCode::Char(c) => {
                                    self.session_state.current_input.push(c);
                                }
                                KeyCode::Backspace => {
                                    self.session_state.current_input.pop();
                                }
                                KeyCode::Esc => {
                                    // Leave sandbox running, just exit session
                                    self.current_screen = AppScreen::SandboxList;
                                    self.input_mode = false;
                                    self.refresh_sandbox_list().await?;
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => {
                            self.current_screen = AppScreen::SandboxList;
                        }
                        _ => {}
                    }
                }
            }
            AppScreen::SandboxSession(sandbox_id) => {
                if self.input_mode {
                    match key.code {
                        KeyCode::Enter => {
                            if !self.session_state.current_input.is_empty() {
                                let command = self.session_state.current_input.clone();
                                self.session_state.current_input.clear();
                                self.execute_command(&command, &sandbox_id).await?;
                            }
                        }
                        KeyCode::Char(c) => {
                            self.session_state.current_input.push(c);
                        }
                        KeyCode::Backspace => {
                            self.session_state.current_input.pop();
                        }
                        KeyCode::Esc => {
                            self.current_screen = AppScreen::SandboxList;
                            self.input_mode = false;
                            self.refresh_sandbox_list().await?;
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn colorize_trajectory_line(line: &str) -> Line<'static> {
        if line.trim_start().starts_with("$ ") {
            // Command line - green and bold
            Line::from(line.to_string()).style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
        } else if line.trim_start().starts_with("(exit code:") {
            // Exit code - yellow
            Line::from(line.to_string()).style(Style::default().fg(Color::Yellow))
        } else if line.trim().is_empty() {
            // Empty line
            Line::from(line.to_string())
        } else {
            // Regular output - cyan
            Line::from(line.to_string()).style(Style::default().fg(Color::Cyan))
        }
    }

    fn colorize_session_line(line: &str) -> Line<'static> {
        if line.starts_with("$ ") {
            // Command line - green and bold
            Line::from(line.to_string()).style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
        } else if line.starts_with("(exit code:") {
            // Exit code - yellow
            Line::from(line.to_string()).style(Style::default().fg(Color::Yellow))
        } else if line.starts_with("Sandbox") && line.contains("started successfully") {
            // Success message - green
            Line::from(line.to_string()).style(Style::default().fg(Color::Green))
        } else {
            // Regular output - default color
            Line::from(line.to_string())
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        
        // Clear status message after drawing
        let status = self.status_message.take();
        
        match self.current_screen.clone() {
            AppScreen::SandboxList => self.draw_sandbox_list(frame, area),
            AppScreen::SandboxDetail(sandbox_id) => self.draw_sandbox_detail(frame, area, &sandbox_id),
            AppScreen::NewSandbox => self.draw_new_sandbox(frame, area),
            AppScreen::SandboxSession(sandbox_id) => self.draw_sandbox_session(frame, area, &sandbox_id),
        }
        
        // Draw status message at the bottom
        if let Some(msg) = status {
            let status_area = Rect {
                x: area.x,
                y: area.bottom().saturating_sub(1),
                width: area.width,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(msg).style(Style::default().fg(Color::Yellow)),
                status_area,
            );
        }
    }

    fn draw_sandbox_list(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
            .split(area);

        // Header
        let header = Paragraph::new("SOS - Sandbox Manager")
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(header, chunks[0]);

        // Help text
        let help_text = "↑/↓,k/j: Navigate | gg: Top | G: Bottom | Enter: View Details | n: New Sandbox | r: Refresh | q: Quit";
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center);
        
        // Sandbox list
        let list_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)].as_ref())
            .split(chunks[1]);

        if self.sandbox_list.is_empty() {
            let empty_msg = Paragraph::new("No sandboxes found. Press 'n' to create a new one.")
                .style(Style::default().fg(Color::Gray))
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title("Sandboxes"));
            frame.render_widget(empty_msg, list_chunks[0]);
        } else {
            // Update scroll based on viewport
            let viewport_height = list_chunks[0].height as usize;
            self.update_list_scroll_with_viewport(viewport_height);

            let visible_items: Vec<ListItem> = self
                .sandbox_list
                .iter()
                .enumerate()
                .skip(self.list_scroll_offset)
                .take(viewport_height.saturating_sub(2)) // Account for borders
                .map(|(i, sandbox)| {
                    let content = format!(
                        "{:<8} | {:<20} | {:<10} | {}",
                        &sandbox.id[..8.min(sandbox.id.len())],
                        sandbox.image,
                        sandbox.status,
                        if sandbox.setup_commands.is_empty() { 
                            "none".to_string() 
                        } else if sandbox.setup_commands.len() > 30 {
                            format!("{}...", &sandbox.setup_commands[..27])
                        } else {
                            sandbox.setup_commands.clone()
                        }
                    );
                    let style = if i == self.selected_sandbox {
                        Style::default().bg(Color::Blue).fg(Color::White)
                    } else {
                        Style::default()
                    };
                    ListItem::new(content).style(style)
                })
                .collect();

            let title = format!(
                "Sandboxes ({}/{}) - gg:top G:bottom", 
                self.selected_sandbox + 1, 
                self.sandbox_list.len()
            );

            let list = List::new(visible_items)
                .block(Block::default().borders(Borders::ALL).title(title))
                .highlight_style(Style::default().bg(Color::Blue));

            frame.render_widget(list, list_chunks[0]);
        }
        
        frame.render_widget(help, list_chunks[1]);
    }

    fn draw_sandbox_detail(&self, frame: &mut Frame, area: Rect, sandbox_id: &str) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(1),
            ].as_ref())
            .split(area);

        // Header
        let title = format!("Sandbox Details - {}", &sandbox_id[..8.min(sandbox_id.len())]);
        let header = Paragraph::new(title)
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(header, chunks[0]);

        // Trajectory
        let trajectory_title = if self.detail_state.formatted {
            "Trajectory (Formatted)"
        } else {
            "Trajectory (Raw JSON)"
        };
        
        let lines: Vec<Line> = self.detail_state.trajectory
            .lines()
            .skip(self.detail_state.scroll_offset)
            .take(chunks[1].height.saturating_sub(2) as usize)
            .map(|line| Self::colorize_trajectory_line(line))
            .collect();

        let trajectory = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(trajectory_title))
            .wrap(Wrap { trim: true });
        frame.render_widget(trajectory, chunks[1]);

        // Help
        let help_text = "↑/↓: Scroll | t: Toggle Format | s: Start Session | x: Stop & Remove | Esc: Back";
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center);
        frame.render_widget(help, chunks[2]);
    }

    fn draw_new_sandbox(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(1),
            ].as_ref())
            .split(area);

        // Header
        let header = Paragraph::new("New Sandbox")
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(header, chunks[0]);

        // Content based on step
        match &self.new_sandbox_state.step {
            NewSandboxStep::EnterImage => {
                let form_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Min(0),
                    ].as_ref())
                    .split(chunks[1]);

                let image_input = Paragraph::new(self.new_sandbox_state.image.as_str())
                    .style(Style::default().fg(Color::Yellow))
                    .block(Block::default().borders(Borders::ALL).title("Docker Image"));
                frame.render_widget(image_input, form_chunks[0]);

                let instructions = Paragraph::new("Enter the Docker image name (e.g., ubuntu:latest, python:3.9)\nPress Enter to continue, Esc to cancel")
                    .style(Style::default().fg(Color::Gray))
                    .block(Block::default().borders(Borders::ALL).title("Instructions"));
                frame.render_widget(instructions, form_chunks[1]);
            }
            NewSandboxStep::EnterSetupCommands => {
                let form_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(5),
                        Constraint::Length(3),
                        Constraint::Min(0),
                    ].as_ref())
                    .split(chunks[1]);

                // Show existing setup commands
                let existing_commands = self.new_sandbox_state.setup_commands.join("\n");
                let commands_display = Paragraph::new(existing_commands)
                    .style(Style::default())
                    .block(Block::default().borders(Borders::ALL).title("Setup Commands"));
                frame.render_widget(commands_display, form_chunks[0]);

                // Current command input
                let current_input = Paragraph::new(self.new_sandbox_state.current_command.as_str())
                    .style(Style::default().fg(Color::Yellow))
                    .block(Block::default().borders(Borders::ALL).title("Add Command"));
                frame.render_widget(current_input, form_chunks[1]);

                let instructions = Paragraph::new("Enter setup commands one by one. Press Enter after each command.\nPress Enter on empty line to finish and create sandbox.\nEsc to cancel")
                    .style(Style::default().fg(Color::Gray))
                    .block(Block::default().borders(Borders::ALL).title("Instructions"));
                frame.render_widget(instructions, form_chunks[2]);
            }
            NewSandboxStep::Creating => {
                let creating = Paragraph::new("Creating sandbox...")
                    .style(Style::default().fg(Color::Yellow))
                    .alignment(Alignment::Center)
                    .block(Block::default().borders(Borders::ALL));
                frame.render_widget(creating, chunks[1]);
            }
            NewSandboxStep::SessionReady => {
                self.draw_session_content(frame, chunks[1]);
            }
        }

        // Help
        let help_text = "Follow the prompts | Esc: Cancel and return to main menu";
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center);
        frame.render_widget(help, chunks[2]);
    }

    fn draw_sandbox_session(&self, frame: &mut Frame, area: Rect, sandbox_id: &str) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(1),
            ].as_ref())
            .split(area);

        // Header
        let title = format!("Session - {}", &sandbox_id[..8.min(sandbox_id.len())]);
        let header = Paragraph::new(title)
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(header, chunks[0]);

        self.draw_session_content(frame, chunks[1]);

        // Help
        let help_text = "Type commands and press Enter | Esc: Exit session (leave sandbox running)";
        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center);
        frame.render_widget(help, chunks[2]);
    }

    fn draw_session_content(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(3)].as_ref())
            .split(area);

        // History
        let history_lines: Vec<Line> = self.session_state.history
            .iter()
            .skip(self.session_state.scroll_offset)
            .take(chunks[0].height.saturating_sub(2) as usize)
            .map(|line| Self::colorize_session_line(line))
            .collect();

        let history = Paragraph::new(history_lines)
            .block(Block::default().borders(Borders::ALL).title("Output"))
            .wrap(Wrap { trim: true });
        frame.render_widget(history, chunks[0]);

        // Input
        let input = Paragraph::new(self.session_state.current_input.as_str())
            .style(Style::default().fg(Color::Yellow))
            .block(Block::default().borders(Borders::ALL).title("Command"));
        frame.render_widget(input, chunks[1]);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let server_url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://localhost:3000".to_string());
    
    let mut app = App::new(server_url);
    
    // Initial data load
    let _ = app.refresh_sandbox_list().await;

    // Main loop
    loop {
        terminal.draw(|f| app.draw(f))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                app.handle_key_event(key).await?;
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
} 