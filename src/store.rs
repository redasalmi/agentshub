//! The database, for any thread to use without waiting on SQLite: what it is
//! asked to do runs on threads of its own, and the answer comes back as a
//! future. Writes run one at a time on one thread, in the order they were
//! asked for. Reads run beside them, and beside each other, each on a
//! connection of its own, and each sees every write asked for before it.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use anyhow::Context as _;
use smol::channel::{self, Sender};

pub use self::db::{
    Chat, Hit, Imported, Item, PendingImport, Project, Session, Transcript, now, path,
    prepare_import,
};
// Only tests reach the database other than through the store.
#[cfg(not(test))]
use self::db::Db;
#[cfg(test)]
pub use self::db::Db;
use crate::agent::Agent;
use crate::settings::Settings;
use crate::transcript;
use crate::worktree::Worktree;

mod db;

/// What recording a reported session changed.
pub struct Recorded {
    pub session_id: i64,
    /// The session the chat was in before, if it switched away from one,
    /// and what says, once it is read, whether its history shows anything
    /// new.
    pub left: Option<(i64, channel::Receiver<anyhow::Result<bool>>)>,
}

/// The database as startup opened it.
pub struct Opened {
    pub store: Store,
    /// Keeps a second Agents Hub off the database while it is held.
    pub lock: Option<fs::File>,
    pub imported: Option<Imported>,
    pub settings: Settings,
    pub projects: Vec<Project>,
    /// Why the database can't be used, in which case an empty one in memory
    /// stands in for it.
    pub broken: Option<String>,
}

/// Opens the database where it lives, with the settings saved in it. Taking
/// the lock, finishing an import and migrating, which first backs the
/// database up, can each take a while, so they run on a thread of their own.
pub async fn open() -> Opened {
    let (store, lock, imported, broken) = smol::unblock(|| {
        let opened = db::open_locked().and_then(|(db, lock, imported)| {
            let store = Store::new(db).context("Couldn't read the database")?;
            Ok((store, lock, imported))
        });
        match opened {
            Ok((store, lock, imported)) => (store, Some(lock), imported, None),
            Err(err) => {
                eprintln!("{err:#}");
                let db = Db::open_in_memory().expect("SQLite opens in memory");
                let store = Store::new(db).expect("the database's threads start");
                (store, None, None, Some(format!("{err:#}")))
            }
        }
    })
    .await;
    let settings = store
        .settings()
        .await
        .inspect_err(|err| eprintln!("can't read the settings: {err:#}"))
        .unwrap_or_default();
    let projects = store
        .projects()
        .await
        .inspect_err(|err| eprintln!("can't list the projects: {err:#}"))
        .unwrap_or_default();
    Opened {
        store,
        lock,
        imported,
        settings,
        projects,
        broken,
    }
}

/// A change the user makes to the projects or chats, which shows before it
/// is saved.
#[derive(Clone, Debug)]
pub enum Change {
    /// Pins or unpins a chat, which goes to the top of its new group.
    Pin { chat: i64, pinned: bool },
    /// Moves a chat to where `to` is, among the chats pinned as it is.
    MoveChat { chat: i64, to: i64 },
    /// Moves a project to where `to` is.
    MoveProject { project: i64, to: i64 },
    /// Titles a chat, or with an empty title, titles it after its sessions.
    Rename { chat: i64, title: String },
    /// Archives a chat, or takes it out of the archive where it was.
    ArchiveChat { chat: i64, archived: bool },
    /// Archives a project, or takes it out of the archive where it was.
    ArchiveProject { project: i64, archived: bool },
    /// Brings back a chat and its project, whichever is archived, each at
    /// the top of its list.
    RestoreChat(i64),
    /// Brings back a project at the top of the list, if it is archived.
    RestoreProject(i64),
}

impl Change {
    /// Makes the change to `projects`, as listed, the way saving it changes
    /// how they are listed. The title a chat gets back from its sessions is
    /// only known once it's saved.
    pub fn apply(&self, projects: &mut Vec<Project>) {
        let find = |projects: &[Project], id: i64| {
            projects.iter().enumerate().find_map(|(project, each)| {
                let chat = each.chats.iter().position(|chat| chat.id == id)?;
                Some((project, chat))
            })
        };
        match *self {
            Self::Pin { chat, pinned } => {
                let Some((project, at)) = find(projects, chat) else {
                    return;
                };
                let chats = &mut projects[project].chats;
                let mut chat = chats.remove(at);
                chat.pinned = pinned;
                let top = if pinned {
                    0
                } else {
                    chats.iter().take_while(|chat| chat.pinned).count()
                };
                chats.insert(top, chat);
            }
            Self::MoveChat { chat, to } => {
                let (Some((project, from)), Some((into, at))) =
                    (find(projects, chat), find(projects, to))
                else {
                    return;
                };
                let chats = &mut projects[project].chats;
                if project == into && chats[from].pinned == chats[at].pinned {
                    let chat = chats.remove(from);
                    chats.insert(at, chat);
                }
            }
            Self::MoveProject { project, to } => {
                let from = projects.iter().position(|each| each.id == project);
                let at = projects.iter().position(|each| each.id == to);
                if let (Some(from), Some(at)) = (from, at) {
                    let project = projects.remove(from);
                    projects.insert(at, project);
                }
            }
            Self::Rename { chat, ref title } => {
                let title = title.trim();
                if !title.is_empty()
                    && let Some((project, at)) = find(projects, chat)
                {
                    projects[project].chats[at].title = Some(title.to_string());
                }
            }
            Self::ArchiveChat { chat, archived } => {
                if let Some((project, at)) = find(projects, chat) {
                    projects[project].chats[at].archived_at = archived.then(db::now);
                }
            }
            Self::ArchiveProject { project, archived } => {
                if let Some(project) = projects.iter_mut().find(|each| each.id == project) {
                    project.archived_at = archived.then(db::now);
                }
            }
            Self::RestoreChat(chat) => {
                let Some((project, at)) = find(projects, chat) else {
                    return;
                };
                let chats = &mut projects[project].chats;
                if chats[at].archived_at.is_some() {
                    let mut chat = chats.remove(at);
                    chat.archived_at = None;
                    let top = if chat.pinned {
                        0
                    } else {
                        chats.iter().take_while(|chat| chat.pinned).count()
                    };
                    chats.insert(top, chat);
                }
                restore_project(projects, project);
            }
            Self::RestoreProject(project) => {
                if let Some(at) = projects.iter().position(|each| each.id == project) {
                    restore_project(projects, at);
                }
            }
        }
    }
}

/// Brings back the `at`th project at the top of the list, if it is archived.
fn restore_project(projects: &mut Vec<Project>, at: usize) {
    if projects[at].archived_at.is_some() {
        let mut project = projects.remove(at);
        project.archived_at = None;
        projects.insert(0, project);
    }
}

/// Something asked of the database, which sends its own answer.
type Job = Box<dyn FnOnce(&Db) + Send>;

/// How many reads can run at once, so that a slow one, such as a search,
/// doesn't hold up the rest.
const READERS: usize = 2;

#[derive(Clone)]
pub struct Store {
    writes: Sender<Job>,
    /// Each read with how many writes were asked for before it.
    reads: Sender<(u64, Job)>,
    order: Arc<Order>,
    /// How many searches were asked for, and what stops the one under way,
    /// with its number.
    searches: Arc<AtomicU64>,
    searching: Arc<Mutex<Option<(u64, rusqlite::InterruptHandle)>>>,
    /// What each session's transcript read holds while it runs, as two that
    /// started from the same offset would each save what they found.
    transcripts: Arc<Mutex<HashMap<i64, Arc<Mutex<()>>>>>,
}

/// How far the writes have got, which a read waits on.
#[derive(Default)]
struct Order {
    /// Writes asked for so far, held while one is queued so that they are
    /// queued in the order they are counted.
    asked: Mutex<u64>,
    done: Mutex<u64>,
    finished: Condvar,
    /// Sessions that recordings left and that are still being read.
    left: Mutex<usize>,
    read_left: Condvar,
}

impl Store {
    pub fn new(db: Db) -> anyhow::Result<Self> {
        let order = Arc::new(Order::default());
        let (reads, asked) = channel::unbounded::<(u64, Job)>();
        for _ in 0..READERS {
            let db = db.reader()?;
            let asked = asked.clone();
            let order = order.clone();
            thread::Builder::new()
                .name("db-read".into())
                .spawn(move || {
                    while let Ok((after, job)) = asked.recv_blocking() {
                        order.wait_for(after);
                        run(job, &db);
                    }
                })
                .context("couldn't start a thread to read the database on")?;
        }
        let (writes, asked) = channel::unbounded::<Job>();
        let done = order.clone();
        thread::Builder::new()
            .name("db-write".into())
            .spawn(move || {
                while let Ok(job) = asked.recv_blocking() {
                    run(job, &db);
                    done.finish();
                }
            })
            .context("couldn't start a thread to write the database on")?;
        Ok(Self {
            writes,
            reads,
            order,
            searches: Arc::default(),
            searching: Arc::default(),
            transcripts: Arc::default(),
        })
    }

    /// The saved settings. One that can't be read has its default.
    pub fn settings(&self) -> impl Future<Output = anyhow::Result<Settings>> + use<> {
        self.read(|db| Ok(Settings::load(&db.settings()?)))
    }

    /// The projects, in the order the user put them in, with their chats.
    pub fn projects(&self) -> impl Future<Output = anyhow::Result<Vec<Project>>> + use<> {
        self.read(|db| Ok(db.projects()?))
    }

    /// A project's folder, or `None` once it is removed.
    pub fn project_path(
        &self,
        id: i64,
    ) -> impl Future<Output = anyhow::Result<Option<PathBuf>>> + use<> {
        self.read(move |db| Ok(db.project_path(id)?))
    }

    /// A chat's agent and the folder it runs in, or `None` once it is
    /// deleted.
    pub fn chat(
        &self,
        id: i64,
    ) -> impl Future<Output = anyhow::Result<Option<(Agent, PathBuf)>>> + use<> {
        self.read(move |db| Ok(db.chat(id)?))
    }

    /// A chat's sessions, the one it is in last.
    pub fn sessions(
        &self,
        chat_id: i64,
    ) -> impl Future<Output = anyhow::Result<Vec<Session>>> + use<> {
        self.read(move |db| Ok(db.sessions(chat_id)?))
    }

    /// A session's last `limit` items, in conversation order.
    pub fn items(
        &self,
        session_id: i64,
        limit: usize,
    ) -> impl Future<Output = anyhow::Result<Vec<Item>>> + use<> {
        self.read(move |db| Ok(db.items(session_id, limit)?))
    }

    /// Whether a session was prompted, and when its last prompt with a time
    /// was sent, in seconds since the Unix epoch.
    pub fn last_prompt(
        &self,
        session_id: i64,
    ) -> impl Future<Output = anyhow::Result<(bool, Option<i64>)>> + use<> {
        self.read(move |db| Ok(db.last_prompt(session_id)?))
    }

    /// The sessions whose transcripts hold something the database doesn't.
    pub fn transcripts_behind(&self) -> impl Future<Output = anyhow::Result<Vec<i64>>> + use<> {
        self.read(|db| {
            Ok(db
                .read_states()?
                .into_iter()
                .filter(|(_, path, offset, parser)| {
                    path.metadata()
                        .is_ok_and(|file| *parser < transcript::PARSER || file.len() != *offset)
                })
                .map(|(id, ..)| id)
                .collect())
        })
    }

    /// Finds what was said in the chats. A newer search stops this one, which
    /// then finds nothing.
    pub fn search(&self, input: String) -> impl Future<Output = anyhow::Result<Vec<Hit>>> + use<> {
        let search = self.searches.fetch_add(1, Ordering::SeqCst) + 1;
        // Taken under the lock, a handle only stops the search it was saved
        // for: once that one is done, it is gone.
        if let Some((_, running)) = lock(&self.searching).take() {
            running.interrupt();
        }
        let (searches, searching) = (self.searches.clone(), self.searching.clone());
        self.read(move |db| {
            if searches.load(Ordering::SeqCst) != search {
                return Ok(Vec::new());
            }
            *lock(&searching) = Some((search, db.interrupt_handle()));
            let hits = db.search(&input);
            let mut running = lock(&searching);
            if running.as_ref().is_some_and(|(each, _)| *each == search) {
                *running = None;
            }
            match hits {
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::OperationInterrupted =>
                {
                    Ok(Vec::new())
                }
                hits => Ok(hits?),
            }
        })
    }

    /// Reads the lines added to a session's transcript since the last read,
    /// and says whether the history shows anything new. The file is parsed on
    /// a thread of its own, and what it holds is saved with the other writes.
    pub fn read_transcript(
        &self,
        session_id: i64,
    ) -> impl Future<Output = anyhow::Result<bool>> + use<> {
        let store = self.clone();
        smol::unblock(move || store.read_transcript_here(session_id))
    }

    /// The same, on the thread it's called on, which it blocks.
    fn read_transcript_here(&self, session_id: i64) -> anyhow::Result<bool> {
        let reading = {
            let mut transcripts = lock(&self.transcripts);
            transcripts.retain(|_, reading| Arc::strong_count(reading) > 1);
            transcripts.entry(session_id).or_default().clone()
        };
        let _reading = lock(&reading);
        let read = self.read(move |db| Ok(db.transcript(session_id)?));
        let Some(transcript) = smol::block_on(read)? else {
            return Ok(false);
        };
        match transcript::parse(&transcript) {
            Ok(read) => {
                smol::block_on(self.write(move |db| Ok(db.save_read_state(session_id, &read)?)))
            }
            // Its items stay.
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                smol::block_on(self.write(move |db| Ok(db.set_missing(session_id)?)))?;
                Ok(false)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Records that a chat's CLI is in a session, for the given reason. The
    /// session it left, if it left one, is read once that is saved, whether
    /// or not anything waits for the answer.
    pub fn record_session(
        &self,
        chat_id: i64,
        agent: Agent,
        agent_id: String,
        transcript_path: PathBuf,
        reason: Option<String>,
    ) -> impl Future<Output = anyhow::Result<Recorded>> + use<> {
        let store = self.clone();
        self.write(move |db| {
            let recorded = db.record_session(
                chat_id,
                agent,
                &agent_id,
                &transcript_path,
                reason.as_deref(),
            )?;
            Ok(Recorded {
                session_id: recorded.session_id,
                left: recorded.left.map(|left| (left, store.read_left(left))),
            })
        })
    }

    /// Reads a session a recording left, on a thread of its own, counted
    /// until it's done so that `wait_for_writes` waits for it.
    fn read_left(&self, session_id: i64) -> channel::Receiver<anyhow::Result<bool>> {
        *lock(&self.order.left) += 1;
        let (send, read) = channel::bounded(1);
        let store = self.clone();
        smol::unblock(move || {
            let answer =
                panic::catch_unwind(AssertUnwindSafe(|| store.read_transcript_here(session_id)))
                    .unwrap_or_else(|_| {
                        Err(anyhow::anyhow!("reading session {session_id} panicked"))
                    });
            send.try_send(answer).ok();
            *lock(&store.order.left) -= 1;
            store.order.read_left.notify_all();
        })
        .detach();
        read
    }

    /// Marks a session's transcript as gone, and if it wasn't yet, says so in
    /// its history with `notice`.
    pub fn lose_transcript(
        &self,
        session_id: i64,
        notice: &'static str,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        self.write(move |db| {
            if db.set_missing(session_id)? {
                db.add_notice(session_id, notice)?;
            }
            Ok(())
        })
    }

    /// Marks a session as just used, which orders the sidebar.
    pub fn touch_session(
        &self,
        session_id: i64,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        self.write(move |db| Ok(db.touch_session(session_id)?))
    }

    /// Adds a project, or brings an archived one back, and gives its id.
    pub fn add_project(&self, path: PathBuf) -> impl Future<Output = anyhow::Result<i64>> + use<> {
        self.write(move |db| Ok(db.add_project(&path)?))
    }

    /// Deletes a project's rows, and its chats', but nothing on disk.
    /// Nothing is deleted once the project is removed, as its id can then
    /// name another folder than `path`.
    pub fn remove_project(
        &self,
        id: i64,
        path: PathBuf,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        self.write(move |db| {
            if db.project_path(id)? == Some(path) {
                db.remove_project(id)?;
            }
            Ok(())
        })
    }

    /// Adds a chat at the top of its project's unpinned chats, running in its
    /// project's folder or in `worktree`, titled `title` unless it's empty,
    /// and gives its id. Its project is brought back if it was archived.
    /// Nothing is added once the project is removed, as its id can then name
    /// another folder than `project`.
    pub fn add_chat(
        &self,
        project_id: i64,
        project: PathBuf,
        agent: Agent,
        worktree: Option<Worktree>,
        title: String,
    ) -> impl Future<Output = anyhow::Result<Option<i64>>> + use<> {
        self.write(move |db| {
            Ok(db.add_chat_to(project_id, &project, agent, worktree.as_ref(), &title)?)
        })
    }

    pub fn delete_chat(&self, id: i64) -> impl Future<Output = anyhow::Result<()>> + use<> {
        self.write(move |db| Ok(db.delete_chat(id)?))
    }

    pub fn change(&self, change: Change) -> impl Future<Output = anyhow::Result<()>> + use<> {
        self.write(move |db| {
            match change {
                Change::Pin { chat, pinned } => db.set_pinned(chat, pinned)?,
                Change::MoveChat { chat, to } => db.move_chat(chat, to)?,
                Change::MoveProject { project, to } => db.move_project(project, to)?,
                Change::Rename { chat, title } => db.rename_chat(chat, &title)?,
                Change::ArchiveChat { chat, archived } => db.set_chat_archived(chat, archived)?,
                Change::ArchiveProject { project, archived } => {
                    db.set_project_archived(project, archived)?
                }
                Change::RestoreChat(chat) => {
                    db.restore_chat(chat)?;
                }
                Change::RestoreProject(project) => {
                    db.restore_project(project)?;
                }
            }
            Ok(())
        })
    }

    /// Saves a setting that is on or off.
    pub fn set_flag(
        &self,
        key: &'static str,
        value: bool,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        self.write(move |db| Ok(db.set_flag(key, value)?))
    }

    /// Saves a setting that is one of several words.
    pub fn set_text(
        &self,
        key: &'static str,
        value: &'static str,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        self.write(move |db| Ok(db.set_text(key, value)?))
    }

    /// Saves a setting that is a number.
    pub fn set_number(
        &self,
        key: &'static str,
        value: f64,
    ) -> impl Future<Output = anyhow::Result<()>> + use<> {
        self.write(move |db| Ok(db.set_number(key, value)?))
    }

    /// Writes a consistent copy of the database to `path`, outside its own
    /// folder.
    pub fn export(&self, path: PathBuf) -> impl Future<Output = anyhow::Result<()>> + use<> {
        self.write(move |db| db.export(&path))
    }

    /// Waits until every write asked for so far is done, with the reads of
    /// the sessions their recordings left, as quitting does so that none is
    /// lost. It blocks the thread it's called on.
    pub fn wait_for_writes(&self) {
        loop {
            let asked = *lock(&self.order.asked);
            self.order.wait_for(asked);
            drop(
                self.order
                    .read_left
                    .wait_while(lock(&self.order.left), |left| *left > 0)
                    .unwrap_or_else(PoisonError::into_inner),
            );
            // What those reads found is saved with writes of their own.
            if *lock(&self.order.asked) == asked {
                return;
            }
        }
    }

    fn write<T, F>(&self, job: F) -> impl Future<Output = anyhow::Result<T>> + use<T, F>
    where
        T: Send + 'static,
        F: FnOnce(&Db) -> anyhow::Result<T> + Send + 'static,
    {
        let (job, answer) = answered(job);
        let mut asked = lock(&self.order.asked);
        *asked += 1;
        // If it can't be sent, it is dropped, and the answer says so.
        self.writes.try_send(job).ok();
        drop(asked);
        answer
    }

    fn read<T, F>(&self, job: F) -> impl Future<Output = anyhow::Result<T>> + use<T, F>
    where
        T: Send + 'static,
        F: FnOnce(&Db) -> anyhow::Result<T> + Send + 'static,
    {
        let after = *lock(&self.order.asked);
        let (job, answer) = answered(job);
        self.reads.try_send((after, job)).ok();
        answer
    }
}

impl Order {
    fn finish(&self) {
        *lock(&self.done) += 1;
        self.finished.notify_all();
    }

    /// Waits until the first `asked` writes are done. They are done in the
    /// order they were asked for.
    fn wait_for(&self, asked: u64) {
        let done = lock(&self.done);
        drop(
            self.finished
                .wait_while(done, |done| *done < asked)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// `job` as the threads run it, and its answer.
fn answered<T, F>(job: F) -> (Job, impl Future<Output = anyhow::Result<T>>)
where
    T: Send + 'static,
    F: FnOnce(&Db) -> anyhow::Result<T> + Send + 'static,
{
    let (send, answer) = channel::bounded(1);
    let job: Job = Box::new(move |db| {
        send.try_send(job(db)).ok();
    });
    let answer = async move {
        answer
            .recv()
            .await
            .context("the database stopped before it answered")?
    };
    (job, answer)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A job that panics fails only what asked for it, which gets no answer.
fn run(job: Job, db: &Db) {
    panic::catch_unwind(AssertUnwindSafe(|| job(db))).ok();
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    use super::*;

    fn store() -> Store {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        Store::new(Db::open(&dir.join("agentshub.db")).unwrap()).unwrap()
    }

    fn project_names(store: &Store) -> Vec<String> {
        smol::block_on(store.projects())
            .unwrap()
            .into_iter()
            .map(|project| project.name)
            .collect()
    }

    /// Projects as the sidebar lists them, with whether each project and
    /// chat is archived rather than when.
    type Shape = Vec<(i64, bool, Vec<(i64, bool, bool, Option<String>)>)>;

    fn shape(projects: Vec<Project>) -> Shape {
        projects
            .into_iter()
            .map(|project| {
                let chats = project
                    .chats
                    .into_iter()
                    .map(|chat| {
                        let archived = chat.archived_at.is_some();
                        (chat.id, chat.pinned, archived, chat.title)
                    })
                    .collect();
                (project.id, project.archived_at.is_some(), chats)
            })
            .collect()
    }

    #[test]
    fn a_change_shows_as_it_is_saved() {
        let changes = |a: i64, b: i64, c: i64, chats: &[i64]| {
            let [one, two, three, four, five] = chats else {
                unreachable!()
            };
            let (one, two, three, four, five) = (*one, *two, *three, *four, *five);
            vec![
                Change::Pin {
                    chat: three,
                    pinned: true,
                },
                Change::Pin {
                    chat: two,
                    pinned: false,
                },
                Change::MoveChat {
                    chat: one,
                    to: three,
                },
                Change::MoveChat {
                    chat: three,
                    to: one,
                },
                Change::MoveChat { chat: two, to: one },
                Change::MoveChat {
                    chat: one,
                    to: five,
                },
                Change::MoveProject { project: a, to: b },
                Change::MoveProject { project: b, to: a },
                Change::Rename {
                    chat: one,
                    title: " Plans ".into(),
                },
                Change::ArchiveChat {
                    chat: one,
                    archived: true,
                },
                Change::ArchiveChat {
                    chat: four,
                    archived: false,
                },
                Change::ArchiveProject {
                    project: b,
                    archived: true,
                },
                Change::ArchiveProject {
                    project: c,
                    archived: false,
                },
                Change::RestoreChat(four),
                Change::RestoreChat(five),
                Change::RestoreChat(one),
                Change::RestoreProject(c),
                Change::RestoreProject(a),
            ]
        };
        for at in 0.. {
            let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            let db = Db::open(&dir.join("agentshub.db")).unwrap();
            let c = db.add_project(Path::new("/home/me/c")).unwrap();
            let b = db.add_project(Path::new("/home/me/b")).unwrap();
            let a = db.add_project(Path::new("/home/me/a")).unwrap();
            let mut chats: Vec<i64> = (0..4)
                .map(|_| db.add_chat(a, Agent::Claude, None).unwrap())
                .collect();
            chats.push(db.add_chat(c, Agent::Claude, None).unwrap());
            db.set_pinned(chats[1], true).unwrap();
            db.set_chat_archived(chats[3], true).unwrap();
            db.set_chat_archived(chats[4], true).unwrap();
            db.set_project_archived(c, true).unwrap();
            let Some(change) = changes(a, b, c, &chats).into_iter().nth(at) else {
                break;
            };
            let mut shown = db.projects().unwrap();
            change.apply(&mut shown);
            let store = Store::new(db.clone()).unwrap();
            smol::block_on(store.change(change.clone())).unwrap();
            assert_eq!(shape(shown), shape(db.projects().unwrap()), "{change:?}");
        }
    }

    #[test]
    fn writes_run_in_the_order_they_were_asked_for() {
        let store = store();
        let added: Vec<_> = ["a", "b", "c"]
            .map(|name| store.add_project(PathBuf::from(format!("/home/me/{name}"))))
            .into_iter()
            .collect();
        let ids: Vec<i64> = added
            .into_iter()
            .map(|added| smol::block_on(added).unwrap())
            .collect();
        assert!(ids.is_sorted(), "{ids:?}");
        assert_eq!(project_names(&store), ["c", "b", "a"]);
    }

    #[test]
    fn a_read_sees_the_writes_asked_for_before_it() {
        let store = store();
        let slow = store.write(|db| {
            thread::sleep(Duration::from_millis(200));
            Ok(db.add_project(&PathBuf::from("/home/me/slow"))?)
        });
        let after = store.projects();
        let projects = smol::block_on(after).unwrap();
        assert_eq!(projects.len(), 1);
        smol::block_on(slow).unwrap();
    }

    #[test]
    fn reads_run_beside_a_slow_read() {
        let store = store();
        let slow = store.read(|_| {
            thread::sleep(Duration::from_secs(2));
            Ok(())
        });
        let started = std::time::Instant::now();
        smol::block_on(store.projects()).unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        smol::block_on(slow).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reads_open_a_database_whose_path_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = std::env::temp_dir()
            .join(format!("agentshub-{}", uuid::Uuid::new_v4()))
            .join(std::ffi::OsStr::from_bytes(b"\xff"));
        let store = Store::new(Db::open(&dir.join("agentshub.db")).unwrap()).unwrap();
        smol::block_on(store.add_project(PathBuf::from("/home/me/project"))).unwrap();
        assert_eq!(project_names(&store), ["project"]);
    }

    #[test]
    fn adds_a_chat_only_to_the_folder_it_was_asked_for() {
        let store = store();
        let project = PathBuf::from("/home/me/project");
        let id = smol::block_on(store.add_project(project.clone())).unwrap();
        let elsewhere = store.add_chat(
            id,
            PathBuf::from("/home/me/other"),
            Agent::Claude,
            None,
            String::new(),
        );
        assert_eq!(smol::block_on(elsewhere).unwrap(), None);
        let added = store.add_chat(id, project, Agent::Claude, None, String::new());
        assert!(smol::block_on(added).unwrap().is_some());
    }

    #[test]
    fn removes_a_project_only_while_it_names_the_folder_it_was_asked_for() {
        let store = store();
        let project = PathBuf::from("/home/me/project");
        let id = smol::block_on(store.add_project(project.clone())).unwrap();
        smol::block_on(store.remove_project(id, PathBuf::from("/home/me/other"))).unwrap();
        assert_eq!(project_names(&store), ["project"]);
        smol::block_on(store.remove_project(id, project)).unwrap();
        assert!(project_names(&store).is_empty());
    }

    #[test]
    fn waits_for_the_writes_asked_for() {
        let store = store();
        let order = store.order.clone();
        let slow = store.write(|db| {
            thread::sleep(Duration::from_millis(200));
            Ok(db.add_project(&PathBuf::from("/home/me/slow"))?)
        });
        store.wait_for_writes();
        assert_eq!(*lock(&order.done), 1);
        drop(slow);
    }

    #[test]
    fn waits_for_the_session_a_recording_left_to_be_read() {
        let dir = std::env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("agentshub.db")).unwrap();
        let project = db.add_project(&dir).unwrap();
        let chat = db.add_chat(project, Agent::Claude, None).unwrap();
        let (first, second) = (dir.join("first.jsonl"), dir.join("second.jsonl"));
        let left = db
            .record_session(chat, Agent::Claude, "first", &first, Some("startup"))
            .unwrap()
            .session_id;
        let prompt = serde_json::json!({
            "type": "user", "uuid": "1", "parentUuid": null,
            "message": { "role": "user", "content": [{ "type": "text", "text": "kept" }] }
        });
        fs::write(&first, format!("{prompt}\n")).unwrap();
        let store = Store::new(db.clone()).unwrap();
        let recorded = smol::block_on(store.record_session(
            chat,
            Agent::Claude,
            "second".into(),
            second,
            Some("clear".into()),
        ))
        .unwrap();
        assert_eq!(recorded.left.map(|(id, _)| id), Some(left));
        // Nothing waits for the read but quitting.
        store.wait_for_writes();
        assert_eq!(db.items(left, 10).unwrap()[0].payload["text"], "kept");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_job_that_panics_fails_only_itself() {
        let store = store();
        let write = store.write::<(), _>(|_| panic!("write"));
        let read = store.read::<(), _>(|_| panic!("read"));
        assert!(smol::block_on(write).is_err());
        assert!(smol::block_on(read).is_err());
        smol::block_on(store.add_project(PathBuf::from("/home/me/project"))).unwrap();
        assert_eq!(project_names(&store), ["project"]);
    }
}
