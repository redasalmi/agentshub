//! Importing a database: checking, copying, confirming and backing up.

use anyhow::bail;

use super::*;

/// Where a database picked for import waits, beside the database, for the
/// user to see what it holds and confirm. Each pick has a copy of its own,
/// named after this and an id, so one can't delete or replace another's.
const PENDING_IMPORT: &str = "import-pending";

/// Where a confirmed import waits for the next start to put it in place of the
/// database. Nothing else may have the database open meanwhile.
const STAGED_IMPORT: &str = "import";

/// What became of a database the user imported.
pub enum Imported {
    /// It replaced the database, which was backed up to this file first, when
    /// there was one.
    Replaced(Option<PathBuf>),
    /// It couldn't, and the database is as it was.
    Failed(anyhow::Error),
}

/// A database picked for import, checked and copied beside the database,
/// waiting for the user to confirm or turn it down.
#[derive(Clone)]
pub struct PendingImport {
    pub(super) copy: PathBuf,
}

impl PendingImport {
    /// Leaves the copy for the next start to put it in place of the database.
    /// The last one confirmed is the one used.
    pub fn confirm(&self) -> anyhow::Result<()> {
        let path = path().context("there is no home folder to keep the database in")?;
        self.confirm_beside(&path)
    }

    pub(super) fn confirm_beside(&self, path: &Path) -> anyhow::Result<()> {
        fs::rename(&self.copy, beside(path, STAGED_IMPORT))
            .context("its checked copy is gone; import it again")
    }

    /// Deletes the copy, unless it was confirmed.
    pub fn discard(&self) {
        remove_database(&self.copy);
    }
}

/// Checks that `source` is a database Agents Hub wrote, and copies it beside
/// the database, brought up to this version's schema. Returns the copy, and
/// what it holds for the user to see before confirming. The error says why it
/// can't be used.
pub fn prepare_import(source: &Path) -> anyhow::Result<(PendingImport, Vec<Project>)> {
    let path = path().context("there is no home folder to keep the database in")?;
    prepare_import_beside(&path, source)
}

pub(super) fn prepare_import_beside(
    path: &Path,
    source: &Path,
) -> anyhow::Result<(PendingImport, Vec<Project>)> {
    if let (Ok(source), Ok(path)) = (fs::canonicalize(source), fs::canonicalize(path))
        && source == path
    {
        bail!("it is the database Agents Hub is using");
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
        restrict(dir, 0o700);
    }
    let pending = PendingImport {
        copy: beside(
            path,
            &format!("{PENDING_IMPORT}-{}", uuid::Uuid::new_v4().simple()),
        ),
    };
    match copy_checked(source, &pending.copy) {
        Ok(projects) => Ok((pending, projects)),
        Err(err) => {
            pending.discard();
            Err(err)
        }
    }
}

/// Copies a database Agents Hub wrote to `target`, checked and migrated, and
/// lists what it holds.
fn copy_checked(source: &Path, target: &Path) -> anyhow::Result<Vec<Project>> {
    let conn = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("it can't be opened")?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    check_import(&conn)?;
    // A consistent copy, even of a database another program is writing.
    vacuum_into(&conn, target)?;
    drop(conn);

    let mut conn = Connection::open(target)?;
    // One file, without a write-ahead log beside it, is what gets moved in
    // place of the database.
    conn.pragma_update(None, "journal_mode", "DELETE")?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    migrate(&mut conn, version)?;
    let db = Db {
        write: Arc::new(Mutex::new(conn)),
        read: None,
        path: None,
        dir: None,
    };
    Ok(db.projects()?)
}

/// Checks that a database is whole, was written by Agents Hub, and that this
/// version can read it. The error says what is wrong with it.
pub(super) fn check_import(conn: &Connection) -> anyhow::Result<()> {
    let version: i64 = match conn.pragma_query_value(None, "user_version", |row| row.get(0)) {
        Ok(version) => version,
        Err(rusqlite::Error::SqliteFailure(err, _))
            if err.code == rusqlite::ErrorCode::NotADatabase =>
        {
            bail!("it isn't a database")
        }
        Err(err) => return Err(err).context("it can't be read"),
    };
    // Agents Hub only ever counts up from 0, and a negative version would
    // count past every migration.
    if version <= 0 {
        bail!("it isn't an Agents Hub database");
    }
    if version > SCHEMA {
        bail!("it was written by a newer Agents Hub (schema {version}, this one knows {SCHEMA})");
    }
    let integrity: String = conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .context("it is damaged")?;
    if integrity != "ok" {
        bail!("it is damaged: {integrity}");
    }
    if shape(conn)? != expected_shape(version)? {
        bail!("it isn't an Agents Hub database: its tables aren't the ones Agents Hub writes");
    }
    if conn.prepare("PRAGMA foreign_key_check")?.exists([])? {
        bail!("it is damaged: some of its rows refer to rows that aren't there");
    }
    let mut agents = conn.prepare("SELECT agent FROM chats UNION SELECT agent FROM sessions")?;
    for label in agents.query_map([], |row| row.get::<_, String>(0))? {
        let label = label?;
        if Agent::from_label(&label).is_none() {
            bail!("it has chats with an agent this Agents Hub doesn't know: {label}");
        }
    }
    Ok(())
}

/// The tables, columns and indexes of a database, which one Agents Hub wrote
/// has exactly as the migrations up to its version made them.
fn shape(conn: &Connection) -> Result<Vec<String>> {
    let objects = conn
        .prepare(
            "SELECT type, name, tbl_name FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;
    let mut columns =
        conn.prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1) ORDER BY cid")?;
    let mut shape = Vec::new();
    for (kind, name, table) in objects {
        shape.push(format!("{kind} {name} on {table}"));
        if kind == "table" {
            for column in columns.query_map([&name], |row| {
                Ok(format!(
                    "{name}.{} {} not null: {}, key: {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, i64>(3)?
                ))
            })? {
                shape.push(column?);
            }
        }
    }
    Ok(shape)
}

/// The shape of a database Agents Hub wrote at `version`.
fn expected_shape(version: i64) -> Result<Vec<String>> {
    let conn = Connection::open_in_memory()?;
    for migration in &MIGRATIONS[..usize::try_from(version).unwrap_or(usize::MAX)] {
        conn.execute_batch(migration)?;
    }
    shape(&conn)
}

/// Puts a confirmed import in place of the database at `path`, once the
/// database is backed up beside it. Also deletes the imports the user never
/// confirmed. Nothing may have the database open.
pub(super) fn take_import(path: &Path) -> Option<Imported> {
    for copy in pending_imports(path) {
        remove_database(&copy);
    }
    let staged = beside(path, STAGED_IMPORT);
    if !staged.exists() {
        return None;
    }
    let replaced = replace_database(path, &staged);
    if replaced.is_err() {
        remove_database(&staged);
    }
    Some(match replaced {
        Ok(backup) => Imported::Replaced(backup),
        Err(err) => Imported::Failed(err),
    })
}

fn replace_database(path: &Path, staged: &Path) -> anyhow::Result<Option<PathBuf>> {
    let backup = if path.exists() {
        let stamp = jiff::Zoned::now().strftime("%Y-%m-%d-%H%M%S");
        let backup = beside(path, &format!("before-import-{stamp}.bak"));
        let conn = Connection::open(path).context("can't open the database it replaces")?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // Reads through the write-ahead log, so nothing committed is lost.
        vacuum_into(&conn, &backup).context("can't back up the database it replaces")?;
        drop(conn);
        Some(backup)
    } else {
        None
    };
    // SQLite would apply the old database's write-ahead log to the new one.
    for suffix in ["wal", "shm", "journal"] {
        let file = beside_with(path, "-", suffix);
        match fs::remove_file(&file) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(err).with_context(|| format!("can't remove {}", file.display()));
            }
            _ => {}
        }
    }
    fs::rename(staged, path).context("can't move it in place of the database")?;
    restrict(path, 0o600);
    Ok(backup)
}

/// The copies of picked databases beside the database at `path`, whether or
/// not they were confirmed.
pub(super) fn pending_imports(path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let mut prefix = name.to_owned();
    prefix.push(format!(".{PENDING_IMPORT}-"));
    let prefix = prefix.as_encoded_bytes();
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name())
        // What SQLite keeps beside each copy goes with it.
        .filter(|name| {
            let name = name.as_encoded_bytes();
            name.strip_prefix(prefix)
                .is_some_and(|id| !id.is_empty() && !id.contains(&b'-'))
        })
        .map(|name| dir.join(name))
        .collect()
}

/// `path` with `.suffix` after its name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    beside_with(path, ".", suffix)
}

fn beside_with(path: &Path, separator: &str, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(separator);
    name.push(suffix);
    PathBuf::from(name)
}

/// Deletes a database and what SQLite keeps beside it, where there is any.
fn remove_database(path: &Path) {
    fs::remove_file(path).ok();
    for suffix in ["wal", "shm", "journal"] {
        fs::remove_file(beside_with(path, "-", suffix)).ok();
    }
}
