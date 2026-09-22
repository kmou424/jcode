//! `/open` (and `jcode open`) two-pane directory browser.
//!
//! The left pane lists the current directory — directories first, then
//! files — and the right pane shows a read-only summary (entry counts, git
//! branch/dirty/ahead-behind) of the listed directory. Enter launches a new
//! jcode rooted at the highlighted directory (or the listed directory when
//! a file is highlighted); the App/CLI layer owns the actual launch.
//!
//! The overlay itself is backend-agnostic: it emits `DirBrowserAction`s and
//! consumes `DirListing`s. Locally the App loads listings on a background
//! thread; over SSH the App relays `browse_dir` sideband ops. That keeps the
//! picker single-path while remote reads run bridge-side on the remote host.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::path::{Path, PathBuf};

const PANEL_BG: Color = Color::Rgb(24, 28, 40);
const PANEL_BORDER: Color = Color::Rgb(90, 95, 110);
const SELECTED_BG: Color = Color::Rgb(38, 42, 56);
const ACCENT: Color = Color::Rgb(186, 139, 255);
const MUTED: Color = Color::Rgb(140, 146, 163);
const MUTED_DARK: Color = Color::Rgb(100, 106, 122);
const FILE_FG: Color = Color::Rgb(160, 165, 178);

const OVERLAY_PERCENT_X: u16 = 78;
const OVERLAY_PERCENT_Y: u16 = 70;

/// One directory entry row in the left pane.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
}

/// Compact git summary for the listed directory.
#[derive(Debug, Clone)]
pub struct DirGitSummary {
    pub branch: Option<String>,
    pub dirty: usize,
    pub ahead: usize,
    pub behind: usize,
}

/// Result of loading one directory, whichever backend produced it.
#[derive(Debug)]
pub struct DirListing {
    /// Absolute (or remote-resolved) path that was actually listed.
    pub path: String,
    pub entries: Vec<DirEntry>,
    pub git: Option<DirGitSummary>,
}

impl DirListing {
    /// Convert a `browse_dir` sideband reply into a listing. Shared by the
    /// in-TUI `/open` remote path and the `jcode --ssh <host> open`
    /// pre-launch picker so both read the same reply shape.
    pub fn from_ssh(
        path: String,
        entries: Vec<jcode_app_core::ssh_ops::SshDirEntry>,
        git: Option<jcode_app_core::ssh_ops::SshDirGitSummary>,
    ) -> Self {
        Self {
            path,
            entries: entries
                .into_iter()
                .map(|entry| DirEntry {
                    name: entry.name,
                    is_dir: entry.is_dir,
                    is_symlink: entry.is_symlink,
                })
                .collect(),
            git: git.map(|git| DirGitSummary {
                branch: git.branch,
                dirty: git.dirty,
                ahead: git.ahead,
                behind: git.behind,
            }),
        }
    }
}

/// What the App/CLI driver should do after a key press.
#[derive(Debug, Clone, PartialEq)]
pub enum DirBrowserAction {
    Continue,
    /// Esc/q: close the overlay (or exit the standalone picker).
    Close,
    /// Navigate: load entries for this path.
    Load(String),
    /// Enter: open a new jcode rooted at this path.
    OpenAt(String),
}

/// Directories first, then files, both alphabetical. Shared by the local
/// reader and mirrored by the remote `browse_dir` op so both backends hand
/// the picker identical ordering.
pub fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
}

/// `base.join(name)` rendered for whichever path style `base` uses. Remote
/// paths are always posix on the bridge, so this stays `/`-based.
pub fn join_path(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Parent of a `/`-style path; None at the filesystem root.
pub fn parent_path(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(index) => Some(trimmed[..index].to_string()),
        None => None,
    }
}

/// Load `path` from the local filesystem, following `~` and relative
/// segments against `cwd`. Used by the in-TUI picker on a blocking thread
/// and by the standalone `jcode open` CLI directly.
pub fn load_local(path: &str, cwd: &Path) -> Result<DirListing, String> {
    let resolved = resolve_local_path(path, cwd);
    let read_dir = std::fs::read_dir(&resolved)
        .map_err(|error| format!("Failed to list {}: {error}", resolved.display()))?;
    let mut entries: Vec<DirEntry> = read_dir
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            let file_type = entry.file_type().ok();
            let is_symlink = file_type.is_some_and(|t| t.is_symlink());
            let is_dir = if is_symlink {
                entry.metadata().map(|m| m.is_dir()).unwrap_or(false)
            } else {
                file_type.is_some_and(|t| t.is_dir())
            };
            DirEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                is_dir,
                is_symlink,
            }
        })
        .collect();
    sort_entries(&mut entries);
    Ok(DirListing {
        path: resolved.display().to_string(),
        entries,
        git: local_dir_git_summary(&resolved),
    })
}

fn resolve_local_path(path: &str, cwd: &Path) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(path));
    }
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    }
}

/// Git summary for `dir` via `git -C`. Mirrors the remote bridge's
/// `remote_dir_git_summary` so both panes show the same fields.
fn local_dir_git_summary(dir: &Path) -> Option<DirGitSummary> {
    use std::process::Command;

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()
    };

    let in_repo = git(&["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !in_repo {
        return None;
    }

    let branch = git(&["branch", "--show-current"]).map(|o| {
        let b = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if b.is_empty() { "HEAD".to_string() } else { b }
    });

    let dirty = git(&["status", "--porcelain"])
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|line| line.len() >= 3)
                .count()
        })
        .unwrap_or(0);

    let (ahead, behind) = git(&["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .filter(|o| o.status.success())
        .and_then(|o| {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let mut parts = text.split('\t');
            Some((
                parts.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                parts.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            ))
        })
        .unwrap_or((0, 0));

    Some(DirGitSummary {
        branch,
        dirty,
        ahead,
        behind,
    })
}

/// Two-pane directory browser overlay. Constructed in `Loading` state; the
/// driver feeds `apply_listing` results as loads complete.
pub struct DirBrowser {
    /// Path of the directory currently listed.
    pub path: String,
    entries: Vec<DirEntry>,
    selected: usize,
    loading: bool,
    error: Option<String>,
    git: Option<DirGitSummary>,
    /// Remote (SSH) mode: paths are remote-host paths and Enter spawns an
    /// SSH attach instead of a local session.
    pub remote: bool,
}

impl DirBrowser {
    /// `start` is where browsing begins (session working dir); the first
    /// `Load(start)` should be issued by the driver right after opening.
    pub fn new(start: String, remote: bool) -> Self {
        Self {
            path: start,
            entries: Vec::new(),
            selected: 0,
            loading: true,
            error: None,
            git: None,
            remote,
        }
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// The highlighted entry's absolute path when it is a directory.
    pub fn selected_dir(&self) -> Option<String> {
        let entry = self.entries.get(self.selected)?;
        entry.is_dir.then(|| join_path(&self.path, &entry.name))
    }

    /// Apply a completed load. Errors stay inside the overlay so the user
    /// can navigate back out (Right arrow still works after a permission
    /// error, for example).
    pub fn apply_listing(&mut self, result: Result<DirListing, String>) {
        self.loading = false;
        match result {
            Ok(listing) => {
                self.path = listing.path;
                self.entries = listing.entries;
                self.git = listing.git;
                self.error = None;
                self.selected = 0;
            }
            Err(error) => {
                self.error = Some(error);
            }
        }
    }

    /// Mark that a load was issued (clears the stale error so the
    /// previous directory's failure does not flash while loading).
    pub fn mark_loading(&mut self) {
        self.loading = true;
    }

    /// Handle a key press. Left enters the highlighted directory and Right
    /// goes to the parent — the mapping the feature spec dictates, reversed
    /// from the conventional direction.
    pub fn handle_overlay_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> DirBrowserAction {
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            return DirBrowserAction::Close;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => DirBrowserAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                DirBrowserAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.entries.len() {
                    self.selected += 1;
                }
                DirBrowserAction::Continue
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(10);
                DirBrowserAction::Continue
            }
            KeyCode::PageDown => {
                self.selected = (self.selected + 10).min(self.entries.len().saturating_sub(1));
                DirBrowserAction::Continue
            }
            KeyCode::Home => {
                self.selected = 0;
                DirBrowserAction::Continue
            }
            KeyCode::End => {
                self.selected = self.entries.len().saturating_sub(1);
                DirBrowserAction::Continue
            }
            // Spec-mandated mapping: Left enters the highlighted directory,
            // Right returns to the parent.
            KeyCode::Left => match self.selected_dir() {
                Some(dir) => {
                    self.loading = true;
                    DirBrowserAction::Load(dir)
                }
                None => DirBrowserAction::Continue,
            },
            KeyCode::Right => match parent_path(&self.path) {
                Some(parent) => {
                    self.loading = true;
                    DirBrowserAction::Load(parent)
                }
                None => DirBrowserAction::Continue,
            },
            KeyCode::Backspace => match parent_path(&self.path) {
                Some(parent) => {
                    self.loading = true;
                    DirBrowserAction::Load(parent)
                }
                None => DirBrowserAction::Continue,
            },
            KeyCode::Enter => {
                let target = self.selected_dir().unwrap_or_else(|| self.path.clone());
                DirBrowserAction::OpenAt(target)
            }
            _ => DirBrowserAction::Continue,
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = centered_rect(OVERLAY_PERCENT_X, OVERLAY_PERCENT_Y, frame.area());
        frame.render_widget(Clear, area);

        let title = format!(" Open directory — {} ", self.path);
        let mut footer = vec![
            Span::styled(" Enter ", Style::default().fg(ACCENT)),
            Span::styled(" open here  ", Style::default().fg(MUTED_DARK)),
            Span::styled(" ← ", Style::default().fg(ACCENT)),
            Span::styled(" enter dir  ", Style::default().fg(MUTED_DARK)),
            Span::styled(" → ", Style::default().fg(ACCENT)),
            Span::styled(" parent  ", Style::default().fg(MUTED_DARK)),
            Span::styled(" ↑/↓ ", Style::default().fg(ACCENT)),
            Span::styled(" move  ", Style::default().fg(MUTED_DARK)),
            Span::styled(" Esc ", Style::default().fg(ACCENT)),
            Span::styled(" close ", Style::default().fg(MUTED_DARK)),
        ];
        if self.remote {
            footer.push(Span::styled(
                " · remote (SSH)",
                Style::default().fg(MUTED_DARK),
            ));
        }
        let block = Block::default()
            .title(title)
            .title_bottom(Line::from(footer))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(PANEL_BORDER))
            .style(Style::default().bg(PANEL_BG));
        frame.render_widget(block, area);

        let inner = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(inner);

        self.render_list(frame, panes[0]);
        self.render_info(frame, panes[1]);
    }

    fn render_list(&self, frame: &mut Frame, area: Rect) {
        if self.loading {
            let line = Paragraph::new("  Loading…").style(Style::default().fg(MUTED).bg(PANEL_BG));
            frame.render_widget(line, area);
            return;
        }
        if let Some(error) = &self.error {
            let text = Paragraph::new(format!("  {error}"))
                .style(Style::default().fg(Color::Red).bg(PANEL_BG))
                .wrap(Wrap { trim: true });
            frame.render_widget(text, area);
            return;
        }
        if self.entries.is_empty() {
            let line =
                Paragraph::new("  (empty)").style(Style::default().fg(MUTED_DARK).bg(PANEL_BG));
            frame.render_widget(line, area);
            return;
        }
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .map(|entry| {
                let mut name = entry.name.clone();
                if entry.is_dir {
                    name.push('/');
                }
                if entry.is_symlink {
                    name.push_str(" @");
                }
                let style = if entry.is_dir {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(FILE_FG)
                };
                ListItem::new(name).style(style)
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::default().bg(SELECTED_BG).fg(ACCENT))
            .highlight_symbol("▌ ");
        let mut state = ListState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_info(&self, frame: &mut Frame, area: Rect) {
        let inner = Rect {
            x: area.x + 2,
            ..area
        };
        let dir_count = self.entries.iter().filter(|e| e.is_dir).count();
        let file_count = self.entries.len() - dir_count;
        let mut lines = vec![
            Line::from(Span::styled("Directory", Style::default().fg(MUTED_DARK))),
            Line::from(Span::styled(
                self.path.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("{dir_count}"), Style::default().fg(ACCENT)),
                Span::styled(" directories   ", Style::default().fg(MUTED)),
                Span::styled(format!("{file_count}"), Style::default().fg(ACCENT)),
                Span::styled(" files", Style::default().fg(MUTED)),
            ]),
            Line::from(""),
        ];
        match &self.git {
            Some(git) => {
                lines.push(Line::from(Span::styled(
                    "Git",
                    Style::default().fg(MUTED_DARK),
                )));
                lines.push(Line::from(vec![
                    Span::styled("branch ", Style::default().fg(MUTED)),
                    Span::styled(
                        git.branch.clone().unwrap_or_else(|| "HEAD".to_string()),
                        Style::default().fg(Color::White),
                    ),
                ]));
                let mut stats = vec![Span::styled(
                    format!("{} dirty", git.dirty),
                    Style::default().fg(if git.dirty > 0 { Color::Yellow } else { MUTED }),
                )];
                if git.ahead > 0 {
                    stats.push(Span::styled(
                        format!("  ↑{}", git.ahead),
                        Style::default().fg(Color::Green),
                    ));
                }
                if git.behind > 0 {
                    stats.push(Span::styled(
                        format!("  ↓{}", git.behind),
                        Style::default().fg(Color::Red),
                    ));
                }
                lines.push(Line::from(stats));
            }
            None => {
                lines.push(Line::from(Span::styled(
                    "Not a git repository",
                    Style::default().fg(MUTED_DARK),
                )));
            }
        }
        if let Some(entry) = self.entries.get(self.selected) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Selected",
                Style::default().fg(MUTED_DARK),
            )));
            let kind = if entry.is_dir {
                "directory"
            } else if entry.is_symlink {
                "symlink"
            } else {
                "file"
            };
            lines.push(Line::from(vec![
                Span::styled(entry.name.clone(), Style::default().fg(Color::White)),
                Span::styled(format!("  ({kind})"), Style::default().fg(MUTED_DARK)),
            ]));
            if entry.is_dir {
                lines.push(Line::from(vec![
                    Span::styled("← ", Style::default().fg(ACCENT)),
                    Span::styled("enter  ", Style::default().fg(MUTED)),
                    Span::styled("Enter ", Style::default().fg(ACCENT)),
                    Span::styled("open jcode here", Style::default().fg(MUTED)),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled("Enter ", Style::default().fg(ACCENT)),
                    Span::styled("open jcode in ", Style::default().fg(MUTED)),
                    Span::styled(self.path.clone(), Style::default().fg(MUTED)),
                ]));
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(PANEL_BG))
                .wrap(Wrap { trim: false }),
            inner,
        );
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> DirEntry {
        DirEntry {
            name: name.to_string(),
            is_dir,
            is_symlink: false,
        }
    }

    #[test]
    fn sort_entries_dirs_first_then_alphabetical() {
        let mut entries = vec![
            entry("zebra.txt", false),
            entry("beta", true),
            entry("alpha.txt", false),
            entry("alpha", true),
        ];
        sort_entries(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta", "alpha.txt", "zebra.txt"]);
    }

    #[test]
    fn join_path_handles_trailing_slash() {
        assert_eq!(join_path("/tmp", "a"), "/tmp/a");
        assert_eq!(join_path("/tmp/", "a"), "/tmp/a");
        assert_eq!(join_path("/", "a"), "/a");
    }

    #[test]
    fn parent_path_unix_semantics() {
        assert_eq!(parent_path("/a/b"), Some("/a".to_string()));
        assert_eq!(parent_path("/a/b/"), Some("/a".to_string()));
        assert_eq!(parent_path("/a"), Some("/".to_string()));
        assert_eq!(parent_path("/"), None);
    }

    #[test]
    fn arrow_keys_use_spec_mapping() {
        let mut browser = DirBrowser::new("/".to_string(), false);
        browser.apply_listing(Ok(DirListing {
            path: "/".to_string(),
            entries: vec![entry("subdir", true), entry("file.txt", false)],
            git: None,
        }));
        // Left enters the highlighted directory.
        assert_eq!(
            browser.handle_overlay_key(KeyCode::Left, KeyModifiers::NONE),
            DirBrowserAction::Load("/subdir".to_string())
        );
        // Right goes to the parent (root has none).
        browser.loading = false;
        assert_eq!(
            browser.handle_overlay_key(KeyCode::Right, KeyModifiers::NONE),
            DirBrowserAction::Continue
        );
        // Down onto the file, Enter opens the listed directory instead.
        browser.handle_overlay_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            browser.handle_overlay_key(KeyCode::Enter, KeyModifiers::NONE),
            DirBrowserAction::OpenAt("/".to_string())
        );
    }

    #[test]
    fn enter_on_dir_opens_that_dir() {
        let mut browser = DirBrowser::new("/home".to_string(), false);
        browser.apply_listing(Ok(DirListing {
            path: "/home".to_string(),
            entries: vec![entry("user", true)],
            git: None,
        }));
        assert_eq!(
            browser.handle_overlay_key(KeyCode::Enter, KeyModifiers::NONE),
            DirBrowserAction::OpenAt("/home/user".to_string())
        );
    }

    #[test]
    fn load_local_lists_and_sorts() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("zdir")).unwrap();
        std::fs::create_dir(tmp.path().join("adir")).unwrap();
        std::fs::write(tmp.path().join("file.txt"), b"x").unwrap();
        let listing = load_local(".", tmp.path()).unwrap();
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["adir", "zdir", "file.txt"]);
        assert!(listing.entries[0].is_dir);
        assert!(!listing.entries[2].is_dir);
    }
}
