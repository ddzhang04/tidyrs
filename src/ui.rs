use crate::plan::{Action, Group, GroupKind, Plan};
use crate::report;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Tabs};
use std::io;
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum Progress {
    Walked(usize),
    HashStart { total: usize },
    Hashed { done: usize, total: usize },
    Done(Vec<Group>),
    Error(String),
}

#[derive(Debug, Clone, Default)]
struct ScanState {
    walked: usize,
    hash_done: usize,
    hash_total: usize,
    error: Option<String>,
}

enum Mode {
    Scanning(ScanState),
    Browsing,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiOutcome {
    Quit,
    Save(Plan),
    Execute(Plan),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Duplicates,
    Clusters,
}

#[derive(Debug, Clone)]
struct Row {
    group_idx: usize,
    chosen: Action,
    expanded: bool,
    file_cursor: usize,
}

pub struct App {
    groups: Vec<Group>,
    rows_dup: Vec<Row>,
    rows_cluster: Vec<Row>,
    tab: Tab,
    selected_dup: usize,
    selected_cluster: usize,
    execute_allowed: bool,
    confirm_execute: bool,
    mode: Mode,
}

impl App {
    pub fn new(groups: Vec<Group>, execute_allowed: bool) -> Self {
        let mut rows_dup = Vec::new();
        let mut rows_cluster = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            let row = Row {
                group_idx: i,
                chosen: g.suggested.clone(),
                expanded: false,
                file_cursor: keeper_index(g),
            };
            match g.kind {
                GroupKind::Duplicate => rows_dup.push(row),
                GroupKind::NameCluster => rows_cluster.push(row),
            }
        }
        Self {
            groups,
            rows_dup,
            rows_cluster,
            tab: Tab::Duplicates,
            selected_dup: 0,
            selected_cluster: 0,
            execute_allowed,
            confirm_execute: false,
            mode: Mode::Browsing,
        }
    }

    pub fn empty_scanning(execute_allowed: bool) -> Self {
        Self {
            groups: Vec::new(),
            rows_dup: Vec::new(),
            rows_cluster: Vec::new(),
            tab: Tab::Duplicates,
            selected_dup: 0,
            selected_cluster: 0,
            execute_allowed,
            confirm_execute: false,
            mode: Mode::Scanning(ScanState::default()),
        }
    }

    pub fn apply_progress(&mut self, p: Progress) {
        match (&mut self.mode, p) {
            (Mode::Scanning(s), Progress::Walked(n)) => s.walked = n,
            (Mode::Scanning(s), Progress::HashStart { total }) => {
                s.hash_total = total;
                s.hash_done = 0;
            }
            (Mode::Scanning(s), Progress::Hashed { done, total }) => {
                s.hash_done = done;
                s.hash_total = total;
            }
            (Mode::Scanning(_), Progress::Done(groups)) => {
                self.groups = groups;
                self.rows_dup.clear();
                self.rows_cluster.clear();
                for (i, g) in self.groups.iter().enumerate() {
                    let row = Row {
                        group_idx: i,
                        chosen: g.suggested.clone(),
                        expanded: false,
                        file_cursor: keeper_index(g),
                    };
                    match g.kind {
                        GroupKind::Duplicate => self.rows_dup.push(row),
                        GroupKind::NameCluster => self.rows_cluster.push(row),
                    }
                }
                self.mode = Mode::Browsing;
            }
            (Mode::Scanning(s), Progress::Error(e)) => {
                s.error = Some(e.clone());
                self.mode = Mode::Failed(e);
            }
            _ => {}
        }
    }

    fn is_scanning(&self) -> bool {
        matches!(self.mode, Mode::Scanning(_))
    }

    fn rows(&self) -> &[Row] {
        match self.tab {
            Tab::Duplicates => &self.rows_dup,
            Tab::Clusters => &self.rows_cluster,
        }
    }

    fn rows_mut(&mut self) -> &mut Vec<Row> {
        match self.tab {
            Tab::Duplicates => &mut self.rows_dup,
            Tab::Clusters => &mut self.rows_cluster,
        }
    }

    fn selected(&self) -> usize {
        match self.tab {
            Tab::Duplicates => self.selected_dup,
            Tab::Clusters => self.selected_cluster,
        }
    }

    fn selected_mut(&mut self) -> &mut usize {
        match self.tab {
            Tab::Duplicates => &mut self.selected_dup,
            Tab::Clusters => &mut self.selected_cluster,
        }
    }

    pub fn build_plan(&self, dry_run: bool) -> Plan {
        let mut actions = Vec::new();
        for row in self.rows_dup.iter().chain(self.rows_cluster.iter()) {
            if !matches!(row.chosen, Action::Ignore) {
                actions.push(row.chosen.clone());
            }
        }
        Plan { actions, dry_run }
    }

    fn totals(&self) -> (usize, usize, u64) {
        let mut groups_selected = 0;
        let mut files_affected = 0;
        let mut bytes = 0u64;
        for row in self.rows_dup.iter().chain(self.rows_cluster.iter()) {
            match &row.chosen {
                Action::Ignore => {}
                Action::KeepOne { trash, .. } => {
                    groups_selected += 1;
                    files_affected += trash.len();
                    let g = &self.groups[row.group_idx];
                    let unit = g.files.first().map(|f| f.size).unwrap_or(0);
                    bytes += unit * trash.len() as u64;
                }
                Action::FoldIntoFolder { files, .. } => {
                    groups_selected += 1;
                    files_affected += files.len();
                }
            }
        }
        (groups_selected, files_affected, bytes)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<UiOutcome> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if self.confirm_execute {
            return self.handle_confirm_key(key);
        }

        if self.is_scanning() {
            return match (key.code, key.modifiers) {
                (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => Some(UiOutcome::Quit),
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(UiOutcome::Quit),
                _ => None,
            };
        }

        if matches!(self.mode, Mode::Failed(_)) {
            return Some(UiOutcome::Quit);
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => Some(UiOutcome::Quit),
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(UiOutcome::Quit),
            (KeyCode::Tab, _) => {
                self.tab = match self.tab {
                    Tab::Duplicates => Tab::Clusters,
                    Tab::Clusters => Tab::Duplicates,
                };
                None
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                let len = self.rows().len();
                if len > 0 {
                    let s = self.selected_mut();
                    *s = (*s + 1).min(len - 1);
                }
                None
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                let s = self.selected_mut();
                if *s > 0 {
                    *s -= 1;
                }
                None
            }
            (KeyCode::Enter, _) => {
                let i = self.selected();
                if let Some(row) = self.rows_mut().get_mut(i) {
                    row.expanded = !row.expanded;
                }
                None
            }
            (KeyCode::Char('a'), _) => {
                let i = self.selected();
                let group_idx = self.rows().get(i).map(|r| r.group_idx);
                if let Some(gi) = group_idx {
                    let suggested = self.groups[gi].suggested.clone();
                    if let Some(row) = self.rows_mut().get_mut(i) {
                        row.chosen = cycle_action(&row.chosen, &suggested);
                    }
                }
                None
            }
            (KeyCode::Char(' '), _) => {
                let i = self.selected();
                let group_idx = self.rows().get(i).map(|r| r.group_idx);
                if let Some(gi) = group_idx {
                    let suggested = self.groups[gi].suggested.clone();
                    if let Some(row) = self.rows_mut().get_mut(i) {
                        row.chosen = if matches!(row.chosen, Action::Ignore) {
                            suggested
                        } else {
                            Action::Ignore
                        };
                    }
                }
                None
            }
            (KeyCode::Char('J'), _) => {
                let i = self.selected();
                let group_idx = self.rows().get(i).map(|r| r.group_idx);
                let n = group_idx.map(|gi| self.groups[gi].files.len()).unwrap_or(0);
                if let Some(row) = self.rows_mut().get_mut(i) {
                    if row.expanded && n > 0 {
                        row.file_cursor = (row.file_cursor + 1).min(n - 1);
                    }
                }
                None
            }
            (KeyCode::Char('K'), _) => {
                let i = self.selected();
                if let Some(row) = self.rows_mut().get_mut(i) {
                    if row.expanded && row.file_cursor > 0 {
                        row.file_cursor -= 1;
                    }
                }
                None
            }
            (KeyCode::Char('m'), _) => {
                let i = self.selected();
                let group_idx = self.rows().get(i).map(|r| r.group_idx);
                if let Some(gi) = group_idx {
                    let cursor = self.rows().get(i).map(|r| r.file_cursor).unwrap_or(0);
                    let new_action = rebuild_keep_one(&self.groups[gi], cursor);
                    if let Some(row) = self.rows_mut().get_mut(i) {
                        row.chosen = new_action;
                    }
                }
                None
            }
            (KeyCode::Char('o'), _) => {
                let i = self.selected();
                if let Some(row) = self.rows().get(i) {
                    let group = &self.groups[row.group_idx];
                    if let Some(file) = group.files.get(row.file_cursor) {
                        let _ = open_in_system(&file.path);
                    }
                }
                None
            }
            (KeyCode::Char('w'), _) => Some(UiOutcome::Save(self.build_plan(true))),
            (KeyCode::Char('x'), _) => {
                if self.execute_allowed {
                    self.confirm_execute = true;
                }
                None
            }
            _ => None,
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> Option<UiOutcome> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.confirm_execute = false;
                Some(UiOutcome::Execute(self.build_plan(false)))
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.confirm_execute = false;
                None
            }
            _ => None,
        }
    }
}

fn keeper_index(group: &Group) -> usize {
    match &group.suggested {
        Action::KeepOne { keep, .. } => group
            .files
            .iter()
            .position(|f| &f.path == keep)
            .unwrap_or(0),
        _ => 0,
    }
}

fn rebuild_keep_one(group: &Group, keeper_idx: usize) -> Action {
    let keep = group.files[keeper_idx].path.clone();
    let trash = group
        .files
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != keeper_idx)
        .map(|(_, f)| f.path.clone())
        .collect();
    Action::KeepOne { keep, trash }
}

fn open_in_system(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let cmd = "open";
    std::process::Command::new(cmd).arg(path).spawn().map(|_| ())
}

fn cycle_action(current: &Action, suggested: &Action) -> Action {
    match (current, suggested) {
        (Action::KeepOne { .. }, _) => Action::Ignore,
        (Action::FoldIntoFolder { .. }, _) => Action::Ignore,
        (Action::Ignore, s) => s.clone(),
    }
}

pub fn run(groups: Vec<Group>, execute_allowed: bool) -> Result<UiOutcome> {
    let (tx, rx) = mpsc::channel();
    tx.send(Progress::Done(groups)).ok();
    drop(tx);
    run_loop(App::empty_scanning(execute_allowed), rx)
}

pub fn run_with_scan<F>(execute_allowed: bool, scan: F) -> Result<UiOutcome>
where
    F: FnOnce(mpsc::Sender<Progress>) + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || scan(tx));
    run_loop(App::empty_scanning(execute_allowed), rx)
}

fn run_loop(mut app: App, rx: mpsc::Receiver<Progress>) -> Result<UiOutcome> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let outcome = loop {
        while let Ok(p) = rx.try_recv() {
            app.apply_progress(p);
        }
        terminal.draw(|f| draw(f, &app))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if let Some(out) = app.handle_key(key) {
                    break out;
                }
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(outcome)
}

fn draw(f: &mut ratatui::Frame, app: &App) {
    match &app.mode {
        Mode::Scanning(s) => return draw_scanning(f, s),
        Mode::Failed(e) => return draw_failed(f, e),
        Mode::Browsing => {}
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(f.area());

    let titles = vec!["Duplicates", "Clusters"];
    let selected_tab = match app.tab {
        Tab::Duplicates => 0,
        Tab::Clusters => 1,
    };
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title("tidy"))
        .select(selected_tab)
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).fg(Color::Yellow));
    f.render_widget(tabs, chunks[0]);

    let rows = app.rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| group_list_item(row, &app.groups[row.group_idx]))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("groups"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(app.selected().min(rows.len() - 1)));
    }
    f.render_stateful_widget(list, chunks[1], &mut state);

    let (groups_sel, files_aff, bytes) = app.totals();
    let mode = if app.execute_allowed { "execute" } else { "dry-run" };
    let status = format!(
        " {} groups • {} files • {:.2} MB reclaimable │ mode: {} │ Tab=switch ↑↓=group J/K=file m=keep o=open a=cycle Space=ignore Enter=expand w=save x=execute q=quit",
        groups_sel,
        files_aff,
        bytes as f64 / 1_000_000.0,
        mode,
    );
    let bar = Paragraph::new(status).block(Block::default().borders(Borders::ALL));
    f.render_widget(bar, chunks[2]);

    if app.confirm_execute {
        draw_confirm_modal(f, files_aff, bytes);
    }
}

fn draw_scanning(f: &mut ratatui::Frame, s: &ScanState) {
    let area = centered_rect(70, 40, f.area());
    f.render_widget(Clear, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(area);

    let outer = Block::default().borders(Borders::ALL).title("scanning");
    f.render_widget(outer, area);

    let walked = Paragraph::new(format!("walked: {} files", s.walked))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(walked, chunks[0]);

    let ratio = if s.hash_total == 0 {
        0.0
    } else {
        (s.hash_done as f64 / s.hash_total as f64).clamp(0.0, 1.0)
    };
    let label = if s.hash_total == 0 {
        "preparing…".to_string()
    } else {
        format!("hashing {} / {}", s.hash_done, s.hash_total)
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title("hash"))
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio(ratio)
        .label(label);
    f.render_widget(gauge, chunks[1]);

    let hint = Paragraph::new("press q to abort").alignment(ratatui::layout::Alignment::Center);
    f.render_widget(hint, chunks[2]);
}

fn draw_failed(f: &mut ratatui::Frame, msg: &str) {
    let area = centered_rect(60, 30, f.area());
    f.render_widget(Clear, area);
    let p = Paragraph::new(format!("scan failed:\n\n{msg}\n\npress any key to quit"))
        .block(Block::default().borders(Borders::ALL).title("error"))
        .alignment(ratatui::layout::Alignment::Center);
    f.render_widget(p, area);
}

fn group_list_item<'a>(row: &'a Row, group: &'a Group) -> ListItem<'a> {
    let marker = match &row.chosen {
        Action::KeepOne { .. } => "[del]",
        Action::FoldIntoFolder { .. } => "[fold]",
        Action::Ignore => "[skip]",
    };
    let unit = group.files.first().map(|f| f.size).unwrap_or(0);
    let header = Line::from(vec![
        Span::styled(
            format!("{:6} ", marker),
            Style::default().fg(match &row.chosen {
                Action::KeepOne { .. } => Color::Red,
                Action::FoldIntoFolder { .. } => Color::Cyan,
                Action::Ignore => Color::DarkGray,
            }),
        ),
        Span::raw(format!(
            "{}  {} files  {:.2} MB  ",
            group.label,
            group.files.len(),
            unit as f64 / 1_000_000.0
        )),
    ]);
    let mut lines = vec![header];
    if row.expanded {
        let keep_path = match &row.chosen {
            Action::KeepOne { keep, .. } => Some(keep.clone()),
            _ => None,
        };
        for (i, f) in group.files.iter().enumerate() {
            let is_keep = keep_path.as_ref().map(|k| k == &f.path).unwrap_or(false);
            let is_focused = i == row.file_cursor;
            let prefix = match (is_focused, is_keep) {
                (true, true) => "  ▶ keep ",
                (true, false) => "  ▶ del  ",
                (false, true) => "    keep ",
                (false, false) => "    del  ",
            };
            let style = if is_focused {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if is_keep {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            lines.push(Line::from(Span::styled(
                format!("{}{}", prefix, f.path.display()),
                style,
            )));
        }
    }
    ListItem::new(lines)
}

fn draw_confirm_modal(f: &mut ratatui::Frame, files: usize, bytes: u64) {
    let area = centered_rect(50, 20, f.area());
    f.render_widget(Clear, area);
    let body = format!(
        "Execute plan?\n\n{} files affected, {:.2} MB reclaimable.\n\n[y] yes   [n] no",
        files,
        bytes as f64 / 1_000_000.0,
    );
    let p = Paragraph::new(body)
        .block(Block::default().borders(Borders::ALL).title("confirm"))
        .alignment(ratatui::layout::Alignment::Center);
    f.render_widget(p, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{GroupFile, GroupKind};
    use std::path::PathBuf;

    fn dup_group(label: &str, paths: Vec<&str>, size: u64) -> Group {
        let files: Vec<GroupFile> = paths
            .iter()
            .map(|p| GroupFile {
                path: PathBuf::from(p),
                size,
            })
            .collect();
        let keep = files[0].path.clone();
        let trash = files[1..].iter().map(|f| f.path.clone()).collect();
        Group {
            kind: GroupKind::Duplicate,
            files,
            label: label.into(),
            suggested: Action::KeepOne { keep, trash },
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn special(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn defaults_to_suggested_actions() {
        let app = App::new(vec![dup_group("h:1", vec!["/a", "/b"], 100)], false);
        assert!(matches!(app.rows_dup[0].chosen, Action::KeepOne { .. }));
    }

    #[test]
    fn space_toggles_ignore() {
        let mut app = App::new(vec![dup_group("h:1", vec!["/a", "/b"], 100)], false);
        app.handle_key(key(' '));
        assert!(matches!(app.rows_dup[0].chosen, Action::Ignore));
        app.handle_key(key(' '));
        assert!(matches!(app.rows_dup[0].chosen, Action::KeepOne { .. }));
    }

    #[test]
    fn a_cycles_actions() {
        let mut app = App::new(vec![dup_group("h:1", vec!["/a", "/b"], 100)], false);
        app.handle_key(key('a'));
        assert!(matches!(app.rows_dup[0].chosen, Action::Ignore));
        app.handle_key(key('a'));
        assert!(matches!(app.rows_dup[0].chosen, Action::KeepOne { .. }));
    }

    #[test]
    fn enter_toggles_expanded() {
        let mut app = App::new(vec![dup_group("h:1", vec!["/a", "/b"], 100)], false);
        assert!(!app.rows_dup[0].expanded);
        app.handle_key(special(KeyCode::Enter));
        assert!(app.rows_dup[0].expanded);
    }

    #[test]
    fn navigation_clamps() {
        let groups = vec![
            dup_group("h:1", vec!["/a", "/b"], 100),
            dup_group("h:2", vec!["/c", "/d"], 100),
        ];
        let mut app = App::new(groups, false);
        app.handle_key(special(KeyCode::Up));
        assert_eq!(app.selected(), 0);
        app.handle_key(special(KeyCode::Down));
        assert_eq!(app.selected(), 1);
        app.handle_key(special(KeyCode::Down));
        assert_eq!(app.selected(), 1);
    }

    #[test]
    fn tab_switches_tabs() {
        let mut app = App::new(vec![dup_group("h:1", vec!["/a", "/b"], 100)], false);
        assert_eq!(app.tab, Tab::Duplicates);
        app.handle_key(special(KeyCode::Tab));
        assert_eq!(app.tab, Tab::Clusters);
    }

    #[test]
    fn q_quits() {
        let mut app = App::new(vec![dup_group("h:1", vec!["/a", "/b"], 100)], false);
        let out = app.handle_key(key('q'));
        assert_eq!(out, Some(UiOutcome::Quit));
    }

    #[test]
    fn w_saves_dry_run_plan() {
        let mut app = App::new(vec![dup_group("h:1", vec!["/a", "/b"], 100)], false);
        let out = app.handle_key(key('w'));
        match out {
            Some(UiOutcome::Save(plan)) => {
                assert!(plan.dry_run);
                assert_eq!(plan.actions.len(), 1);
            }
            other => panic!("expected Save, got {other:?}"),
        }
    }

    #[test]
    fn x_requires_execute_flag() {
        let mut app = App::new(vec![dup_group("h:1", vec!["/a", "/b"], 100)], false);
        let out = app.handle_key(key('x'));
        assert_eq!(out, None);
        assert!(!app.confirm_execute);
    }

    #[test]
    fn x_opens_confirm_then_y_executes() {
        let mut app = App::new(vec![dup_group("h:1", vec!["/a", "/b"], 100)], true);
        assert_eq!(app.handle_key(key('x')), None);
        assert!(app.confirm_execute);
        let out = app.handle_key(key('y'));
        match out {
            Some(UiOutcome::Execute(plan)) => {
                assert!(!plan.dry_run);
                assert_eq!(plan.actions.len(), 1);
            }
            other => panic!("expected Execute, got {other:?}"),
        }
    }

    #[test]
    fn confirm_n_cancels() {
        let mut app = App::new(vec![dup_group("h:1", vec!["/a", "/b"], 100)], true);
        app.handle_key(key('x'));
        assert!(app.confirm_execute);
        let out = app.handle_key(key('n'));
        assert_eq!(out, None);
        assert!(!app.confirm_execute);
    }

    #[test]
    fn ignored_groups_omitted_from_plan() {
        let groups = vec![
            dup_group("h:1", vec!["/a", "/b"], 100),
            dup_group("h:2", vec!["/c", "/d"], 100),
        ];
        let mut app = App::new(groups, false);
        app.handle_key(special(KeyCode::Down));
        app.handle_key(key(' '));
        let plan = app.build_plan(true);
        assert_eq!(plan.actions.len(), 1);
    }

    #[test]
    fn totals_sum_only_active_actions() {
        let groups = vec![
            dup_group("h:1", vec!["/a", "/b"], 100), // reclaim 100
            dup_group("h:2", vec!["/c", "/d"], 200), // reclaim 200
        ];
        let mut app = App::new(groups, false);
        let (g, f, b) = app.totals();
        assert_eq!(g, 2);
        assert_eq!(f, 2);
        assert_eq!(b, 300);

        app.handle_key(key(' ')); // ignore first
        let (g, f, b) = app.totals();
        assert_eq!(g, 1);
        assert_eq!(f, 1);
        assert_eq!(b, 200);
    }

    #[test]
    fn scanning_app_starts_empty() {
        let app = App::empty_scanning(false);
        assert!(app.is_scanning());
        assert!(app.groups.is_empty());
    }

    #[test]
    fn progress_updates_scan_state() {
        let mut app = App::empty_scanning(false);
        app.apply_progress(Progress::Walked(42));
        app.apply_progress(Progress::HashStart { total: 10 });
        app.apply_progress(Progress::Hashed { done: 5, total: 10 });
        match &app.mode {
            Mode::Scanning(s) => {
                assert_eq!(s.walked, 42);
                assert_eq!(s.hash_done, 5);
                assert_eq!(s.hash_total, 10);
            }
            _ => panic!("expected Scanning"),
        }
    }

    #[test]
    fn done_transitions_to_browsing() {
        let mut app = App::empty_scanning(false);
        let g = dup_group("h:1", vec!["/a", "/b"], 100);
        app.apply_progress(Progress::Done(vec![g]));
        assert!(matches!(app.mode, Mode::Browsing));
        assert_eq!(app.rows_dup.len(), 1);
    }

    #[test]
    fn q_during_scan_quits() {
        let mut app = App::empty_scanning(false);
        let out = app.handle_key(key('q'));
        assert_eq!(out, Some(UiOutcome::Quit));
    }

    #[test]
    fn other_keys_during_scan_ignored() {
        let mut app = App::empty_scanning(false);
        assert_eq!(app.handle_key(key('a')), None);
        assert_eq!(app.handle_key(key('w')), None);
    }

    #[test]
    fn file_cursor_defaults_to_keeper() {
        let g = dup_group("h:1", vec!["/a", "/b", "/c"], 100);
        let app = App::new(vec![g], false);
        // keeper is /a (index 0)
        assert_eq!(app.rows_dup[0].file_cursor, 0);
    }

    #[test]
    fn shift_j_k_moves_file_cursor_when_expanded() {
        let g = dup_group("h:1", vec!["/a", "/b", "/c"], 100);
        let mut app = App::new(vec![g], false);
        app.handle_key(special(KeyCode::Enter)); // expand
        assert!(app.rows_dup[0].expanded);
        app.handle_key(key('J'));
        assert_eq!(app.rows_dup[0].file_cursor, 1);
        app.handle_key(key('J'));
        assert_eq!(app.rows_dup[0].file_cursor, 2);
        app.handle_key(key('J')); // clamp
        assert_eq!(app.rows_dup[0].file_cursor, 2);
        app.handle_key(key('K'));
        assert_eq!(app.rows_dup[0].file_cursor, 1);
    }

    #[test]
    fn file_cursor_inert_when_collapsed() {
        let g = dup_group("h:1", vec!["/a", "/b", "/c"], 100);
        let mut app = App::new(vec![g], false);
        app.handle_key(key('J'));
        assert_eq!(app.rows_dup[0].file_cursor, 0);
    }

    #[test]
    fn m_marks_focused_file_as_keeper() {
        let g = dup_group("h:1", vec!["/a", "/b", "/c"], 100);
        let mut app = App::new(vec![g], false);
        app.handle_key(special(KeyCode::Enter));
        app.handle_key(key('J')); // cursor on /b
        app.handle_key(key('m'));
        match &app.rows_dup[0].chosen {
            Action::KeepOne { keep, trash } => {
                assert_eq!(keep, &PathBuf::from("/b"));
                assert_eq!(trash.len(), 2);
                assert!(trash.contains(&PathBuf::from("/a")));
                assert!(trash.contains(&PathBuf::from("/c")));
            }
            other => panic!("expected KeepOne, got {other:?}"),
        }
    }

    #[test]
    fn m_recovers_from_ignore() {
        let g = dup_group("h:1", vec!["/a", "/b"], 100);
        let mut app = App::new(vec![g], false);
        app.handle_key(key(' ')); // ignore
        assert!(matches!(app.rows_dup[0].chosen, Action::Ignore));
        app.handle_key(special(KeyCode::Enter));
        app.handle_key(key('J')); // cursor on /b
        app.handle_key(key('m'));
        match &app.rows_dup[0].chosen {
            Action::KeepOne { keep, .. } => assert_eq!(keep, &PathBuf::from("/b")),
            _ => panic!("expected KeepOne after m"),
        }
    }

    // Suppress dead-code warning on report import in non-test build.
    #[allow(dead_code)]
    fn _use_report() {
        let _ = report::reclaimable_bytes;
    }
}
