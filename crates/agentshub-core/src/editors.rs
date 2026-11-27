//! The code editors a chat's folder can be opened in.

use std::ffi::OsString;
use std::io;
use std::iter;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fs, thread};

/// An editor on the built-in list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Editor {
    Zed,
    VsCode,
    VsCodium,
    Cursor,
    SublimeText,
    Emacs,
    Kate,
}

impl Editor {
    /// Every editor, in the order the first installed one is picked.
    pub const ALL: [Self; 7] = [
        Self::Zed,
        Self::VsCode,
        Self::VsCodium,
        Self::Cursor,
        Self::SublimeText,
        Self::Emacs,
        Self::Kate,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Zed => "Zed",
            Self::VsCode => "VS Code",
            Self::VsCodium => "VSCodium",
            Self::Cursor => "Cursor",
            Self::SublimeText => "Sublime Text",
            Self::Emacs => "Emacs",
            Self::Kate => "Kate",
        }
    }

    /// How the settings spell it.
    pub fn key(self) -> &'static str {
        match self {
            Self::Zed => "zed",
            Self::VsCode => "vscode",
            Self::VsCodium => "vscodium",
            Self::Cursor => "cursor",
            Self::SublimeText => "sublime-text",
            Self::Emacs => "emacs",
            Self::Kate => "kate",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|editor| editor.key() == key)
    }

    /// The names of the desktop entries it installs, the likeliest first.
    pub fn desktop_ids(self) -> &'static [&'static str] {
        match self {
            Self::Zed => &["dev.zed.Zed", "zed"],
            Self::VsCode => &["code", "com.visualstudio.code"],
            Self::VsCodium => &["codium", "com.vscodium.codium"],
            Self::Cursor => &["cursor"],
            Self::SublimeText => &["sublime_text", "com.sublimetext.three"],
            Self::Emacs => &["emacs", "org.gnu.emacs"],
            Self::Kate => &["org.kde.kate"],
        }
    }
}

/// What a desktop entry says about starting its app.
pub(crate) struct Entry {
    pub(crate) exec: String,
    pub(crate) icon: Option<String>,
    pub(crate) try_exec: Option<String>,
    /// Whether the entry asks to be left out of menus.
    pub(crate) hidden: bool,
}

/// The `[Desktop Entry]` group of a desktop entry, if it is an application
/// that says how to start it.
pub(crate) fn parse_entry(text: &str) -> Option<Entry> {
    let mut in_group = false;
    let (mut kind, mut exec, mut icon, mut try_exec, mut hidden) = (None, None, None, None, false);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unescape(value.trim());
        match key.trim() {
            "Type" => kind = Some(value),
            "Exec" => exec = Some(value),
            "Icon" => icon = Some(value),
            "TryExec" => try_exec = Some(value),
            "Hidden" | "NoDisplay" => hidden |= value == "true",
            _ => {}
        }
    }
    if kind.as_deref() != Some("Application") {
        return None;
    }
    Some(Entry {
        exec: exec.filter(|exec| !exec.is_empty())?,
        icon: icon.filter(|icon| !icon.is_empty()),
        try_exec: try_exec.filter(|try_exec| !try_exec.is_empty()),
        hidden,
    })
}

/// Undoes the escapes of a desktop entry's string values, leaving the ones
/// `Exec` quoting uses for `split_exec`.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') | None => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

/// The arguments of an `Exec` line, unquoted as the desktop entry spec says.
fn split_exec(exec: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            let escaped = chars.next()?;
                            if !matches!(escaped, '"' | '`' | '$' | '\\') {
                                current.push('\\');
                            }
                            current.push(escaped);
                        }
                        other => current.push(other),
                    }
                }
            }
            other => {
                started = true;
                current.push(other);
            }
        }
    }
    if started {
        args.push(current);
    }
    Some(args)
}

/// The command an `Exec` line runs to open `folder`: the file and URL field
/// codes become the folder, which goes last when there are none, and the
/// other field codes are dropped.
pub fn expand_exec(exec: &str, folder: &Path) -> Option<Vec<String>> {
    let folder = folder.to_str()?;
    let mut took_files = false;
    let mut args: Vec<String> = split_exec(exec)?
        .into_iter()
        .filter_map(|arg| {
            let mut out = String::with_capacity(arg.len());
            let mut chars = arg.chars();
            while let Some(c) = chars.next() {
                if c != '%' {
                    out.push(c);
                    continue;
                }
                match chars.next() {
                    Some('f' | 'F' | 'u' | 'U') => {
                        out.push_str(folder);
                        took_files = true;
                    }
                    Some('%') => out.push('%'),
                    _ => {}
                }
            }
            (!out.is_empty()).then_some(out)
        })
        .collect();
    if args.is_empty() {
        return None;
    }
    if !took_files {
        args.push(folder.to_owned());
    }
    Some(args)
}

/// An editor on the list that is installed, with how to start it.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub editor: Editor,
    pub desktop_id: &'static str,
    pub exec: String,
    pub icon: Option<PathBuf>,
}

/// Where desktop entries and icons are installed, most important first.
pub struct Dirs {
    pub data_home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
}

impl Dirs {
    /// The XDG data folders, with Flatpak's exports even when the session
    /// doesn't list them.
    pub fn from_env() -> Self {
        let set = |name| env::var_os(name).filter(|value| !value.is_empty());
        let data_home = set("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .unwrap_or_default();
        let listed = set("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        let mut data_dirs: Vec<PathBuf> = env::split_paths(&listed)
            .filter(|dir| !dir.as_os_str().is_empty())
            .collect();
        for flatpak in [
            data_home.join("flatpak/exports/share"),
            PathBuf::from("/var/lib/flatpak/exports/share"),
        ] {
            if !data_dirs.contains(&flatpak) {
                data_dirs.push(flatpak);
            }
        }
        Self {
            data_home,
            data_dirs,
        }
    }

    fn all(&self) -> impl Iterator<Item = &Path> {
        iter::once(self.data_home.as_path())
            .chain(self.data_dirs.iter().map(PathBuf::as_path))
            .filter(|dir| !dir.as_os_str().is_empty())
    }
}

/// The editors on the list that are installed, in the list's order.
pub fn detect(dirs: &Dirs) -> Vec<Found> {
    Editor::ALL
        .into_iter()
        .filter_map(|editor| {
            editor
                .desktop_ids()
                .iter()
                .find_map(|&desktop_id| found(editor, desktop_id, dirs))
        })
        .collect()
}

/// The entry named `desktop_id` that comes first, as XDG says, when it can
/// start its app.
fn found(editor: Editor, desktop_id: &'static str, dirs: &Dirs) -> Option<Found> {
    let file = format!("{desktop_id}.desktop");
    let path = dirs
        .all()
        .map(|dir| dir.join("applications").join(&file))
        .find(|path| path.is_file())?;
    let entry = parse_entry(&fs::read_to_string(path).ok()?)?;
    if entry.hidden || !entry.try_exec.as_deref().is_none_or(on_path) {
        return None;
    }
    Some(Found {
        editor,
        desktop_id,
        icon: entry.icon.as_deref().and_then(|icon| find_icon(icon, dirs)),
        exec: entry.exec,
    })
}

fn on_path(program: &str) -> bool {
    let program = Path::new(program);
    if program.is_absolute() {
        return program.is_file();
    }
    env::var_os("PATH")
        .is_some_and(|path| env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// The sizes of an app's icon in the hicolor theme, the largest first.
const ICON_SIZES: [&str; 7] = [
    "512x512", "256x256", "128x128", "96x96", "64x64", "48x48", "32x32",
];

/// The file of the icon an entry names: its path, or the best hicolor icon,
/// or a pixmap.
fn find_icon(icon: &str, dirs: &Dirs) -> Option<PathBuf> {
    let path = Path::new(icon);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    let themed = dirs.all().flat_map(|dir| {
        let hicolor = dir.join("icons/hicolor");
        iter::once(hicolor.join(format!("scalable/apps/{icon}.svg"))).chain(
            ICON_SIZES
                .iter()
                .map(move |size| hicolor.join(format!("{size}/apps/{icon}.png"))),
        )
    });
    let pixmaps = dirs.data_dirs.iter().flat_map(|dir| {
        ["svg", "png"].map(|extension| dir.join(format!("pixmaps/{icon}.{extension}")))
    });
    themed.chain(pixmaps).find(|path| path.is_file())
}

/// A raster icon shrunk to fit `size` pixels with a filter that keeps thin
/// strokes, as PNG. Drawn straight from a file many times larger, an icon such
/// as Zed's loses most of its mark. `None` for an SVG, which scales by itself,
/// or a file that can't be read.
pub fn icon_png(path: &Path, size: u32) -> Option<Vec<u8>> {
    if path.extension().is_some_and(|extension| extension == "svg") {
        return None;
    }
    let icon = image::open(path).ok()?;
    let mut png = Vec::new();
    icon.resize(size, size, image::imageops::FilterType::Lanczos3)
        .write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// The editor to open folders in: the saved one while it is installed, or else
/// the first installed.
pub fn pick(installed: &[Found], saved: Option<Editor>) -> Option<&Found> {
    saved
        .and_then(|saved| installed.iter().find(|found| found.editor == saved))
        .or_else(|| installed.first())
}

/// What a launcher saved for editors with `AGENTSHUB_EDITOR_<NAME>`: each is
/// dropped, and `<NAME>` is set to its value, or unset when it is empty. A dev
/// build with a data folder and session bus of its own gives the user's back
/// this way.
fn editor_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, Option<OsString>)> {
    vars.into_iter()
        .filter_map(|(key, value)| {
            let name = key.to_str()?.strip_prefix("AGENTSHUB_EDITOR_")?.to_owned();
            let value = (!value.is_empty()).then_some(value);
            Some([(key, None), (name.into(), value)])
        })
        .flatten()
        .collect()
}

/// Starts `found` on `folder`, out of this app's process group. `adopt` gets
/// the editor's pid before it is reaped, on a thread that then waits for it,
/// as long as it runs.
pub fn open(found: &Found, folder: &Path, adopt: fn(u32, &str)) -> io::Result<()> {
    let args = expand_exec(&found.exec, folder).ok_or_else(|| {
        io::Error::other(format!("can't read how to start {}", found.editor.name()))
    })?;
    let Some((program, args)) = args.split_first() else {
        return Err(io::Error::other("nothing to start"));
    };
    if !folder.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("the folder {} no longer exists", folder.display()),
        ));
    }
    let mut command = Command::new(program);
    for (name, value) in editor_env(env::vars_os()) {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    let mut child = command
        .args(args)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let desktop_id = found.desktop_id;
    thread::spawn(move || {
        adopt(child.id(), desktop_id);
        child.wait().ok();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// The desktop ids `open` handed a pid to.
    static ADOPTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn record(pid: u32, desktop_id: &str) {
        assert_ne!(pid, 0);
        ADOPTED.lock().unwrap().push(desktop_id.to_owned());
    }

    /// An editor that writes its folder and arguments to `out`, then sleeps
    /// `sleep` seconds.
    fn fake_editor(sleep: u32, desktop_id: &'static str) -> (Found, PathBuf, PathBuf) {
        let base = temp_dir();
        let script = base.join("editor");
        fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$OUT.part\"\nmv \"$OUT.part\" \"$OUT\"\nsleep \"$SLEEP\"\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let out = base.join("out");
        let folder = base.join("a b");
        fs::create_dir_all(&folder).unwrap();
        let found = Found {
            editor: Editor::Zed,
            desktop_id,
            exec: format!(
                "env \"OUT={}\" SLEEP={sleep} \"{}\" %U",
                out.display(),
                script.display()
            ),
            icon: None,
        };
        (found, folder, out)
    }

    fn wait_for(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "{} never appeared",
                path.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn editor_env_restores_what_the_launcher_saved() {
        let os = |s: &str| OsString::from(s);
        let vars = [
            (
                os("AGENTSHUB_EDITOR_XDG_DATA_HOME"),
                os("/home/me/.local/share"),
            ),
            (os("AGENTSHUB_EDITOR_DBUS_SESSION_BUS_ADDRESS"), os("")),
            (os("HOME"), os("/home/me")),
        ];
        assert_eq!(
            editor_env(vars),
            [
                (os("AGENTSHUB_EDITOR_XDG_DATA_HOME"), None),
                (os("XDG_DATA_HOME"), Some(os("/home/me/.local/share"))),
                (os("AGENTSHUB_EDITOR_DBUS_SESSION_BUS_ADDRESS"), None),
                (os("DBUS_SESSION_BUS_ADDRESS"), None),
            ]
        );
    }

    #[test]
    fn open_runs_the_editor_in_the_folder() {
        let (found, folder, out) = fake_editor(0, "test-runs");
        open(&found, &folder, record).unwrap();
        wait_for(&out);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ADOPTED.lock().unwrap().iter().any(|id| id == "test-runs") {
            assert!(Instant::now() < deadline, "the editor was never adopted");
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            fs::read_to_string(&out).unwrap(),
            format!("{0}\n{0}\n", folder.display())
        );
    }

    #[test]
    fn open_returns_before_the_editor_exits() {
        let (found, folder, out) = fake_editor(3, "test-returns");
        let start = Instant::now();
        open(&found, &folder, record).unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        wait_for(&out);
    }

    #[test]
    fn open_fails_for_missing_folder() {
        let (found, _, _) = fake_editor(0, "test-missing");
        let err = open(&found, Path::new("/nonexistent/agentshub"), record).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the folder /nonexistent/agentshub no longer exists"
        );
    }

    fn temp_dir() -> PathBuf {
        let dir = env::temp_dir().join(format!("agentshub-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    fn write(base: &Path, rel: &str, text: &str) {
        let path = base.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn app(exec: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nExec={exec}\n")
    }

    #[test]
    fn detects_in_list_order_and_home_first() {
        let base = temp_dir();
        let dirs = Dirs {
            data_home: base.join("home"),
            data_dirs: vec![base.join("sys")],
        };
        write(
            &base,
            "sys/applications/org.kde.kate.desktop",
            &app("kate %U"),
        );
        write(
            &base,
            "sys/applications/dev.zed.Zed.desktop",
            &app("sys-zed %U"),
        );
        write(
            &base,
            "home/applications/dev.zed.Zed.desktop",
            &app("home-zed %U"),
        );
        let found = detect(&dirs);
        assert_eq!(
            found.iter().map(|f| f.editor).collect::<Vec<_>>(),
            [Editor::Zed, Editor::Kate]
        );
        assert_eq!(found[0].exec, "home-zed %U");
        assert_eq!(found[0].desktop_id, "dev.zed.Zed");
    }

    #[test]
    fn skips_hidden_and_broken_entries() {
        let base = temp_dir();
        let dirs = Dirs {
            data_home: base.join("home"),
            data_dirs: vec![],
        };
        write(
            &base,
            "home/applications/dev.zed.Zed.desktop",
            "[Desktop Entry]\nType=Application\nExec=zed\nHidden=true\n",
        );
        write(&base, "home/applications/zed.desktop", &app("zed2 %U"));
        write(
            &base,
            "home/applications/code.desktop",
            "[Desktop Entry]\nType=Application\nExec=code\nTryExec=/nonexistent/code\n",
        );
        fs::write(base.join("home/applications/cursor.desktop"), [0xff, 0xfe]).unwrap();
        let found = detect(&dirs);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].editor, found[0].desktop_id), (Editor::Zed, "zed"));
    }

    #[test]
    fn finds_icons() {
        let base = temp_dir();
        let dirs = Dirs {
            data_home: base.join("home"),
            data_dirs: vec![base.join("sys")],
        };
        write(&base, "sys/icons/hicolor/48x48/apps/kate.png", "png");
        write(&base, "sys/icons/hicolor/256x256/apps/kate.png", "png");
        write(&base, "sys/icons/hicolor/scalable/apps/code.svg", "<svg/>");
        write(&base, "sys/pixmaps/emacs.png", "png");
        let entry = |exec: &str, icon: &str| format!("{}Icon={icon}\n", app(exec));
        write(
            &base,
            "sys/applications/org.kde.kate.desktop",
            &entry("kate", "kate"),
        );
        write(
            &base,
            "sys/applications/code.desktop",
            &entry("code", "code"),
        );
        write(
            &base,
            "sys/applications/emacs.desktop",
            &entry("emacs", "emacs"),
        );
        let abs = base.join("zed.png");
        fs::write(&abs, "png").unwrap();
        write(
            &base,
            "sys/applications/dev.zed.Zed.desktop",
            &entry("zed", &abs.display().to_string()),
        );
        write(
            &base,
            "sys/applications/cursor.desktop",
            &entry("cursor", "nowhere"),
        );
        let icon = |editor| {
            detect(&dirs)
                .into_iter()
                .find(|f| f.editor == editor)
                .unwrap()
                .icon
        };
        assert_eq!(icon(Editor::Zed), Some(abs));
        assert_eq!(
            icon(Editor::VsCode),
            Some(base.join("sys/icons/hicolor/scalable/apps/code.svg"))
        );
        assert_eq!(
            icon(Editor::Kate),
            Some(base.join("sys/icons/hicolor/256x256/apps/kate.png"))
        );
        assert_eq!(
            icon(Editor::Emacs),
            Some(base.join("sys/pixmaps/emacs.png"))
        );
        assert_eq!(icon(Editor::Cursor), None);
    }

    #[test]
    fn shrinks_raster_icons_to_the_size_asked() {
        let base = temp_dir();
        let big = base.join("big.png");
        image::RgbaImage::from_pixel(512, 256, image::Rgba([20, 20, 20, 255]))
            .save(&big)
            .unwrap();
        let small = image::load_from_memory(&icon_png(&big, 40).unwrap()).unwrap();
        assert_eq!((small.width(), small.height()), (40, 20));
        write(&base, "logo.svg", "<svg/>");
        assert_eq!(icon_png(&base.join("logo.svg"), 40), None);
        assert_eq!(icon_png(&base.join("missing.png"), 40), None);
    }

    #[test]
    fn pick_falls_back_without_saving() {
        let found = |editor| Found {
            editor,
            desktop_id: "x",
            exec: "x".into(),
            icon: None,
        };
        let installed = [found(Editor::Zed), found(Editor::Kate)];
        assert_eq!(
            pick(&installed, Some(Editor::Kate)).unwrap().editor,
            Editor::Kate
        );
        assert_eq!(
            pick(&installed, Some(Editor::Emacs)).unwrap().editor,
            Editor::Zed
        );
        assert_eq!(pick(&installed, None).unwrap().editor, Editor::Zed);
        assert!(pick(&[], Some(Editor::Zed)).is_none());
    }

    #[test]
    fn keys_read_back() {
        for editor in Editor::ALL {
            assert_eq!(Editor::from_key(editor.key()), Some(editor));
        }
        assert_eq!(Editor::from_key("notepad"), None);
    }

    #[test]
    fn parses_the_desktop_entry_group_only() {
        let entry = parse_entry(
            "[Desktop Entry]\nType=Application\nExec=zed %U\nIcon=zed\n\n[Desktop Action new]\nExec=zed --new %U\n",
        )
        .unwrap();
        assert_eq!(entry.exec, "zed %U");
        assert_eq!(entry.icon.as_deref(), Some("zed"));
        assert!(!entry.hidden);
        assert!(parse_entry("[Desktop Entry]\nType=Link\nURL=x\n").is_none());
        assert!(parse_entry("[Desktop Entry]\nType=Application\n").is_none());
        assert!(
            parse_entry("[Desktop Entry]\nType=Application\nExec=x\nNoDisplay=true\n")
                .unwrap()
                .hidden
        );
    }

    #[test]
    fn expands_field_codes() {
        let folder = Path::new("/home/me/my project");
        assert_eq!(
            expand_exec("zed %U", folder).unwrap(),
            ["zed", "/home/me/my project"]
        );
        assert_eq!(
            expand_exec("emacs %F", folder).unwrap(),
            ["emacs", "/home/me/my project"]
        );
        assert_eq!(
            expand_exec("kate -b %u %i %c %k", folder).unwrap(),
            ["kate", "-b", "/home/me/my project"]
        );
        assert_eq!(
            expand_exec("subl", folder).unwrap(),
            ["subl", "/home/me/my project"]
        );
        assert_eq!(
            expand_exec("app --file=%f 100%%", folder).unwrap(),
            ["app", "--file=/home/me/my project", "100%"]
        );
        assert!(expand_exec("", folder).is_none());
    }

    #[test]
    fn expands_flatpak_exec() {
        let exec = "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=code \
                    --file-forwarding com.visualstudio.code @@ %F @@";
        assert_eq!(
            expand_exec(exec, Path::new("/p")).unwrap(),
            [
                "/usr/bin/flatpak",
                "run",
                "--branch=stable",
                "--arch=x86_64",
                "--command=code",
                "--file-forwarding",
                "com.visualstudio.code",
                "@@",
                "/p",
                "@@"
            ]
        );
        let exec = "/usr/bin/flatpak run --command=zed dev.zed.Zed @@u %U @@";
        assert_eq!(
            expand_exec(exec, Path::new("/p")).unwrap(),
            [
                "/usr/bin/flatpak",
                "run",
                "--command=zed",
                "dev.zed.Zed",
                "@@u",
                "/p",
                "@@"
            ]
        );
    }

    #[test]
    fn unquotes_arguments() {
        assert_eq!(
            expand_exec(
                r#""/opt/My Editor/bin/ed" --x "a \"b\" \$c" %U"#,
                Path::new("/p")
            )
            .unwrap(),
            ["/opt/My Editor/bin/ed", "--x", r#"a "b" $c"#, "/p"]
        );
        assert_eq!(
            parse_entry("[Desktop Entry]\nType=Application\nExec=ed\\sx\n")
                .unwrap()
                .exec,
            "ed x"
        );
        assert!(expand_exec(r#""unterminated %U"#, Path::new("/p")).is_none());
    }
}
