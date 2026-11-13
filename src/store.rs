//! The database, for any thread to use without waiting on SQLite: what it is
//! asked to do runs on threads of its own, and the answer comes back as a
//! future. Writes run one at a time on one thread, in the order they were
//! asked for. Reads run beside them, and beside each other, each on a
//! connection of its own, and each sees every write asked for before it.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "nothing reads or writes through it yet")
)]

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;

use anyhow::Context as _;
use smol::channel::{self, Sender};

use crate::db::{Db, Project};

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
}

/// How far the writes have got, which a read waits on.
#[derive(Default)]
struct Order {
    /// Writes asked for so far, held while one is queued so that they are
    /// queued in the order they are counted.
    asked: Mutex<u64>,
    done: Mutex<u64>,
    finished: Condvar,
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
        })
    }

    /// The projects, in the order the user put them in, with their chats.
    pub fn projects(&self) -> impl Future<Output = anyhow::Result<Vec<Project>>> + use<> {
        self.read(|db| Ok(db.projects()?))
    }

    /// Adds a project, or brings an archived one back, and gives its id.
    pub fn add_project(&self, path: PathBuf) -> impl Future<Output = anyhow::Result<i64>> + use<> {
        self.write(move |db| Ok(db.add_project(&path)?))
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
