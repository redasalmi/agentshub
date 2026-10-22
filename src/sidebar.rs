use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use notify::event::ModifyKind;
use notify::{EventKind, RecursiveMode, Watcher as _};

use crate::agent::Agent;
use crate::sessions::{self, Project, Session, SessionStore};

/// CLIs write sessions in bursts while they run, so rescans wait for a pause.
const RESCAN_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone)]
pub enum SidebarEvent {
    OpenSession(Session),
    /// Show the terminal of a new chat, by its key.
    ShowChat(String),
    NewSession(PathBuf),
    SessionsChanged,
}

/// What a rescan has to look at.
#[derive(Debug, PartialEq)]
enum Change {
    /// Files or folders were added, removed or renamed.
    Structure,
    /// Files were written to.
    Files(HashSet<PathBuf>),
    /// Projects were expanded, so the rest of their sessions is read.
    Expanded,
}

/// A running CLI whose session isn't listed yet, as before its first prompt.
pub struct NewChat {
    pub key: String,
    pub agent: Agent,
    pub cwd: PathBuf,
}

pub struct Sidebar {
    projects: Vec<Project>,
    hidden: HashSet<PathBuf>,
    new_chats: Vec<NewChat>,
    expanded: HashSet<PathBuf>,
    /// The id of the session, or the key of the new chat, that is shown.
    active: Option<String>,
    watcher: Option<notify::RecommendedWatcher>,
    watches: HashMap<PathBuf, RecursiveMode>,
    /// What the watcher looks for changes in, which moves as folders appear.
    targets: Arc<Mutex<Vec<PathBuf>>>,
    changes: smol::channel::Sender<Change>,
    _rescan: Task<()>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let targets = Arc::new(Mutex::new(Vec::new()));
        let (changed_tx, changed) = smol::channel::unbounded();
        let watcher = notify::recommended_watcher({
            let targets = targets.clone();
            let changed_tx = changed_tx.clone();
            move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else {
                    return;
                };
                if let Some(change) = changes_sessions(&event, &targets.lock().unwrap()) {
                    changed_tx.try_send(change).ok();
                }
            }
        })
        .inspect_err(|err| eprintln!("can't watch session folders: {err}"))
        .ok();

        let rescan = cx.spawn(async move |this, cx| {
            let mut store = SessionStore::default();
            let mut change = Change::Structure;
            loop {
                let Ok(expanded) = this.update(cx, |this, _| {
                    this.update_watches();
                    this.expanded.clone()
                }) else {
                    break;
                };
                let (projects, hidden);
                (store, projects, hidden) = cx
                    .background_executor()
                    .spawn(async move {
                        store.load(expanded);
                        let projects = match change {
                            Change::Structure => store.scan(),
                            Change::Files(paths) => store.refresh(&paths),
                            Change::Expanded => store.projects(),
                        };
                        let hidden = store.hidden();
                        (store, projects, hidden)
                    })
                    .await;
                if this
                    .update(cx, |this, cx| this.set_projects(projects, hidden, cx))
                    .is_err()
                {
                    break;
                }

                let Ok(first) = changed.recv().await else {
                    break;
                };
                // An expanded project shows its sessions right away.
                if first != Change::Expanded {
                    cx.background_executor().timer(RESCAN_DELAY).await;
                }
                change = std::iter::from_fn(|| changed.try_recv().ok()).fold(
                    first,
                    |change, next| match (change, next) {
                        (Change::Files(mut paths), Change::Files(more)) => {
                            paths.extend(more);
                            Change::Files(paths)
                        }
                        (Change::Expanded, change) | (change, Change::Expanded) => change,
                        _ => Change::Structure,
                    },
                );
            }
        });

        Self {
            projects: Vec::new(),
            hidden: HashSet::new(),
            new_chats: Vec::new(),
            expanded: HashSet::new(),
            active: None,
            watcher,
            watches: HashMap::new(),
            targets,
            changes: changed_tx,
            _rescan: rescan,
        }
    }

    /// Watches each session folder, or while one doesn't exist yet, the
    /// closest folder above it, so that a CLI's first session is noticed.
    fn update_watches(&mut self) {
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        let targets = sessions::watched_paths();
        let mut wanted = HashMap::new();
        for target in &targets {
            if target.is_dir() {
                wanted.insert(target.clone(), RecursiveMode::Recursive);
            } else if let Some(dir) = target.ancestors().skip(1).find(|dir| dir.is_dir()) {
                wanted
                    .entry(dir.to_path_buf())
                    .or_insert(RecursiveMode::NonRecursive);
            }
        }

        self.watches.retain(|path, mode| {
            let keep = wanted.get(path) == Some(mode);
            if !keep {
                watcher.unwatch(path).ok();
            }
            keep
        });
        for (path, mode) in wanted {
            if self.watches.contains_key(&path) {
                continue;
            }
            match watcher.watch(&path, mode) {
                Ok(()) => {
                    self.watches.insert(path, mode);
                }
                Err(err) => eprintln!("can't watch {}: {err}", path.display()),
            }
        }
        *self.targets.lock().unwrap() = targets;
    }

    pub fn sessions(&self) -> impl Iterator<Item = &Session> {
        self.projects
            .iter()
            .flat_map(|project| project.sessions.iter())
    }

    /// Session files that aren't listed because they aren't chats of their own.
    pub fn hidden_sessions(&self) -> &HashSet<PathBuf> {
        &self.hidden
    }

    pub fn set_chats(
        &mut self,
        new_chats: Vec<NewChat>,
        active: Option<String>,
        cx: &mut Context<Self>,
    ) {
        for chat in &new_chats {
            if !self.new_chats.iter().any(|known| known.key == chat.key) {
                self.expand(chat.cwd.clone());
            }
        }
        self.new_chats = new_chats;
        self.active = active;
        cx.notify();
    }

    fn set_projects(
        &mut self,
        projects: Vec<Project>,
        hidden: HashSet<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.projects.is_empty()
            && let Some(first) = projects.first()
        {
            self.expand(first.path.clone());
        }
        self.projects = projects;
        self.hidden = hidden;
        cx.emit(SidebarEvent::SessionsChanged);
        cx.notify();
    }

    /// Collapsed projects keep their sessions, which take a few hundred bytes
    /// each, but their files are no longer read past what lists them.
    fn toggle_project(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expand(path.clone());
        }
        cx.notify();
    }

    fn expand(&mut self, path: PathBuf) {
        if self.expanded.insert(path) {
            self.changes.try_send(Change::Expanded).ok();
        }
    }

    fn render_project(&self, project: &Project, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let expanded = self.expanded.contains(&project.path);
        let path = project.path.clone();
        let new_session_path = project.path.clone();

        v_flex()
            .child(
                h_flex()
                    .id(SharedString::from(
                        project.path.to_string_lossy().into_owned(),
                    ))
                    .h_8()
                    .px_2()
                    .gap_2()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.sidebar_accent))
                    .child(
                        Icon::new(if expanded {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        })
                        .small()
                        .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_medium()
                            .child(project.name.clone()),
                    )
                    .child(
                        Button::new("new-session")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::NewSession(new_session_path.clone()));
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_project(&path, cx))),
            )
            .when(expanded, |this| {
                this.children(
                    self.new_chats
                        .iter()
                        .filter(|chat| chat.cwd == project.path)
                        .map(|chat| {
                            self.render_chat(
                                &chat.key,
                                "New chat".into(),
                                chat.agent,
                                SidebarEvent::ShowChat(chat.key.clone()),
                                cx,
                            )
                        }),
                )
                .children(project.sessions.iter().map(|session| {
                    self.render_chat(
                        &session.id,
                        session.title.clone().into(),
                        session.agent,
                        SidebarEvent::OpenSession(session.clone()),
                        cx,
                    )
                }))
            })
    }

    fn render_chat(
        &self,
        id: &str,
        title: SharedString,
        agent: Agent,
        on_click: SidebarEvent,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let active = self.active.as_deref() == Some(id);

        v_flex()
            .id(SharedString::from(id.to_string()))
            .ml_4()
            .px_2()
            .py_1p5()
            .gap_0p5()
            .rounded(theme.radius)
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(theme.sidebar_accent)
                        .text_color(theme.sidebar_accent_foreground)
                } else {
                    this.hover(|style| style.bg(theme.sidebar_accent.opacity(0.6)))
                }
            })
            .child(div().text_sm().truncate().child(title))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(agent.label()),
            )
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(on_click.clone())))
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // New chats in a folder without listed sessions get a project of their own.
        let mut new_projects: Vec<Project> = Vec::new();
        for chat in &self.new_chats {
            let listed = self
                .projects
                .iter()
                .chain(&new_projects)
                .any(|project| project.path == chat.cwd);
            if !listed {
                new_projects.push(Project::new(chat.cwd.clone(), Vec::new()));
            }
        }

        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .h_10()
                    .px_4()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("Projects"),
            )
            .child(
                v_flex()
                    .id("projects")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_2()
                    .gap_0p5()
                    .children(
                        new_projects
                            .iter()
                            .chain(&self.projects)
                            .map(|project| self.render_project(project, cx)),
                    ),
            )
    }
}

/// What a rescan has to look at after a change under a watched folder, if it
/// can change the listed sessions at all:
/// session files, folders moved in or out with sessions inside them, or a
/// watched path (or a folder above it) that appeared or went away.
fn changes_sessions(event: &notify::Event, targets: &[PathBuf]) -> Option<Change> {
    // Events were dropped, so anything may have changed.
    if event.need_rescan() {
        return Some(Change::Structure);
    }
    let structural = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
    );
    if !structural && !matches!(event.kind, EventKind::Modify(_)) {
        return None;
    }
    let changed: HashSet<PathBuf> = event
        .paths
        .iter()
        .filter(|path| {
            targets.iter().any(|target| {
                target.starts_with(path)
                    || (path.starts_with(target)
                        && (structural || path.extension().is_some_and(|ext| ext == "jsonl")))
            })
        })
        .cloned()
        .collect();
    if changed.is_empty() {
        None
    } else if structural {
        Some(Change::Structure)
    } else {
        Some(Change::Files(changed))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use notify::EventKind;
    use notify::event::{AccessKind, CreateKind, DataChange, Flag, ModifyKind, RenameMode};

    use super::{Change, changes_sessions};

    fn event(kind: EventKind, path: &str) -> notify::Event {
        notify::Event::new(kind).add_path(path.into())
    }

    #[test]
    fn rescans_for_session_changes_only() {
        let targets = [PathBuf::from("/home/me/.claude/projects")];
        let changes = |kind, path| changes_sessions(&event(kind, path), &targets);
        let written = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        let moved = EventKind::Modify(ModifyKind::Name(RenameMode::From));

        assert_eq!(
            changes(written, "/home/me/.claude/projects/p/s.jsonl"),
            Some(Change::Files(
                [PathBuf::from("/home/me/.claude/projects/p/s.jsonl")].into()
            ))
        );
        assert_eq!(
            changes(written, "/home/me/.claude/projects/p/s/tool.txt"),
            None
        );
        assert_eq!(
            changes(moved, "/home/me/.claude/projects/p"),
            Some(Change::Structure)
        );
        assert_eq!(
            changes(EventKind::Create(CreateKind::Folder), "/home/me/.claude"),
            Some(Change::Structure)
        );
        assert_eq!(
            changes(
                EventKind::Access(AccessKind::Any),
                "/home/me/.claude/projects/p/s.jsonl"
            ),
            None
        );
        assert_eq!(changes(moved, "/home/me/other"), None);
        assert_eq!(
            changes_sessions(
                &notify::Event::new(EventKind::Other).set_flag(Flag::Rescan),
                &targets
            ),
            Some(Change::Structure)
        );
    }
}
