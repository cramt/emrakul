//! Apps are installed desktop entries
//! (<https://specifications.freedesktop.org/desktop-entry-spec/latest/>).

use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
};

/// A desktop file ID: the entry's path under `applications/`, with `/` turned
/// into `-`. Two entries with the same ID are the same app, and the one in the
/// earlier data dir wins.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppId(String);

impl AppId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A command line that has a program to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argv {
    pub program: String,
    pub args: Vec<String>,
}

impl Argv {
    pub fn from_vec(argv: Vec<String>) -> Option<Self> {
        let mut argv = argv.into_iter();
        Some(Self {
            program: argv.next()?,
            args: argv.collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub id: AppId,
    pub name: String,
    pub exec: Argv,
    pub quit: Quit,
    /// `Icon=`: a name to look up in the icon theme, or an absolute path.
    pub icon: Option<String>,
    /// Carries an `X-Emrakul-*` key, which only entries nixconf declares for
    /// the TV do. Before anything has been used, these come first on Home.
    pub declared: bool,
    /// `X-Emrakul-Brand=#rrggbb`: the colour of its tile on Home.
    pub brand: Option<Rgb>,
    /// `X-Emrakul-Tv=<profile>`: the TV settings profile it runs under, in
    /// place of the one for apps.
    pub tv_profile: Option<String>,
    /// `X-Emrakul-Back=<key>`: what the controller's B sends in it.
    pub back: Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub [u8; 3]);

impl Rgb {
    /// `#rrggbb`, nothing else.
    fn parse(hex: &str) -> Option<Self> {
        let digits = hex.strip_prefix('#').filter(|d| d.len() == 6)?;
        let channel = |i: usize| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok();
        Some(Self([channel(0)?, channel(2)?, channel(4)?]))
    }
}

/// The key B sends for "back". Apps don't agree: a regular site goes back
/// in its history on Alt+Left, while YouTube's TV UI and Jellyfin's TV
/// layout go back on Escape, and under them Alt+Left walks browser history
/// behind their router's back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Back {
    /// Browser history back. The default.
    #[default]
    AltLeft,
    /// `X-Emrakul-Back=Escape`.
    Escape,
}

impl Back {
    /// The values `X-Emrakul-Back` takes.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "Alt+Left" => Some(Self::AltLeft),
            "Escape" => Some(Self::Escape),
            _ => None,
        }
    }
}

/// How going Home ends an app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quit {
    /// Ask its windows to close (xdg_toplevel.close), then signal it if it
    /// lingers. The default.
    Close,
    /// Run `X-Emrakul-Quit` instead of closing, then signal if it lingers.
    /// Moonlight needs this: killing it leaves the game running on the
    /// gaming desktop, `moonlight quit <host>` ends it properly.
    Run(Argv),
}

/// Every app installed in `data_dirs`' `applications/` folders, in no
/// particular order. Earlier dirs take precedence, as `XDG_DATA_HOME` does
/// over `XDG_DATA_DIRS`.
pub fn discover(data_dirs: &[PathBuf]) -> Vec<App> {
    let mut entries = HashMap::new();
    for dir in data_dirs {
        let root = dir.join("applications");
        let mut files = Vec::new();
        walk(&root, &mut files);
        for path in files {
            let Some(id) = desktop_file_id(&root, &path) else {
                continue;
            };
            if entries.contains_key(&id) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let entry = parse_entry(id.clone(), &text);
            entries.insert(id, entry);
        }
    }
    entries
        .into_values()
        .filter_map(|entry| match entry {
            Entry::App(app) => Some(app),
            Entry::NotAnApp => None,
        })
        .collect()
}

/// `XDG_DATA_HOME` then `XDG_DATA_DIRS`, with the spec's defaults.
pub fn data_dirs() -> Vec<PathBuf> {
    let home = env_dir("XDG_DATA_HOME").or_else(|| Some(env_dir("HOME")?.join(".local/share")));
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(std::env::split_paths(&system).filter(|p| p.is_absolute()))
        .collect()
}

/// An absolute path from the environment. The basedir spec says relative
/// ones are invalid and must be ignored.
pub fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn desktop_file_id(root: &Path, path: &Path) -> Option<AppId> {
    if path.extension()? != "desktop" {
        return None;
    }
    let relative = path.strip_prefix(root).ok()?.to_str()?;
    Some(AppId::new(relative.replace('/', "-")))
}

/// What one desktop entry file says, before deciding whether it is an app.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    App(App),
    /// `Hidden`, `NoDisplay`, `OnlyShowIn` without us, or not an application
    /// at all. Still masks an entry with the same ID in a later data dir.
    NotAnApp,
}

fn parse_entry(id: AppId, text: &str) -> Entry {
    let mut keys = HashMap::new();
    let mut in_main_group = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_main_group = line == "[Desktop Entry]";
        } else if in_main_group && let Some((key, value)) = line.split_once('=') {
            keys.entry(key.trim())
                .or_insert_with(|| unescape(value.trim()));
        }
    }
    let get = |key| keys.get(key).map(String::as_str);
    let yes = |key| get(key) == Some("true");
    // Our desktop name never appears in anyone's OnlyShowIn list.
    let shown_here =
        get("OnlyShowIn").is_none_or(|desktops| desktops.split(';').any(|d| d == "emrakul"));
    if get("Type") != Some("Application") || yes("Hidden") || yes("NoDisplay") || !shown_here {
        return Entry::NotAnApp;
    }
    let Some(name) = get("Name") else {
        return Entry::NotAnApp;
    };
    let command = |key| get(key).and_then(|exec| parse_exec(exec, name));
    let Some(exec) = command("Exec") else {
        return Entry::NotAnApp;
    };
    let back = get("X-Emrakul-Back").map_or(Back::default(), |value| {
        Back::parse(value).unwrap_or_else(|| {
            tracing::warn!(app = %id, value, "unknown X-Emrakul-Back, using Alt+Left");
            Back::default()
        })
    });
    Entry::App(App {
        id,
        name: name.to_owned(),
        exec,
        quit: command("X-Emrakul-Quit").map_or(Quit::Close, Quit::Run),
        icon: get("Icon").filter(|i| !i.is_empty()).map(str::to_owned),
        declared: keys.keys().any(|key| key.starts_with("X-Emrakul-")),
        brand: get("X-Emrakul-Brand").and_then(Rgb::parse),
        tv_profile: get("X-Emrakul-Tv")
            .filter(|p| !p.is_empty())
            .map(str::to_owned),
        back,
    })
}

/// The escapes every string value may use.
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
            Some('\\') => out.push('\\'),
            // Not a string escape: left for Exec's quoting rules (`\"`).
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits an already unescaped Exec value into arguments and expands its
/// field codes. Home never passes files or URLs, so `%f %F %u %U` vanish.
fn parse_exec(exec: &str, name: &str) -> Option<Argv> {
    let mut args = Vec::new();
    let mut chars = exec.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else { break };
        let mut arg = String::new();
        if first == '"' {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => match chars.next_if(|c| matches!(c, '"' | '`' | '$' | '\\')) {
                        Some(escaped) => arg.push(escaped),
                        None => arg.push('\\'),
                    },
                    c => arg.push(c),
                }
            }
            args.push(arg);
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                arg.push(c);
            }
            // Field codes only count outside quotes.
            if let Some(arg) = expand_field_codes(&arg, name) {
                args.push(arg);
            }
        }
    }
    Argv::from_vec(args)
}

/// `None` when the argument was only field codes that expand to nothing, so
/// it disappears rather than becoming an empty argument.
fn expand_field_codes(arg: &str, name: &str) -> Option<String> {
    let mut out = String::with_capacity(arg.len());
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('%', Some(code)) => {
                chars.next();
                match code {
                    '%' => out.push('%'),
                    'c' => out.push_str(name),
                    // Files, URLs, the icon, and the deprecated codes.
                    _ => {}
                }
            }
            (c, _) => out.push(c),
        }
    }
    (!out.is_empty() || arg.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Argv {
        Argv::from_vec(v.iter().map(|s| s.to_string()).collect()).unwrap()
    }

    fn app(text: &str) -> App {
        match parse_entry(AppId::new("test.desktop"), text) {
            Entry::App(app) => app,
            Entry::NotAnApp => panic!("not an app:\n{text}"),
        }
    }

    #[test]
    fn plain_application() {
        assert_eq!(
            app("[Desktop Entry]\nType=Application\nName=Foot\nExec=foot --server\n"),
            App {
                id: AppId::new("test.desktop"),
                name: "Foot".into(),
                exec: argv(&["foot", "--server"]),
                quit: Quit::Close,
                icon: None,
                declared: false,
                brand: None,
                tv_profile: None,
                back: Back::AltLeft,
            }
        );
    }

    #[test]
    fn an_entry_can_name_its_back_key() {
        let back = |extra: &str| {
            app(&format!(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\n{extra}\n"
            ))
            .back
        };
        assert_eq!(back(""), Back::AltLeft);
        assert_eq!(back("X-Emrakul-Back=Escape"), Back::Escape);
        assert_eq!(back("X-Emrakul-Back=Alt+Left"), Back::AltLeft);
        for bad in ["escape", "Esc", "", "Escape;", "BackSpace"] {
            assert_eq!(
                back(&format!("X-Emrakul-Back={bad}")),
                Back::AltLeft,
                "{bad}"
            );
        }
    }

    #[test]
    fn any_x_emrakul_key_marks_an_entry_declared() {
        let plain = app("[Desktop Entry]\nType=Application\nName=Ark\nExec=ark\nIcon=ark\n");
        assert!(!plain.declared);
        assert_eq!(plain.icon.as_deref(), Some("ark"));
        let declared = app(
            "[Desktop Entry]\nType=Application\nName=YouTube\nExec=chromium\nX-Emrakul-Brand=#FF0033\n",
        );
        assert!(declared.declared);
        assert_eq!(declared.brand, Some(Rgb([0xff, 0x00, 0x33])));
    }

    #[test]
    fn an_entry_can_name_its_tv_profile() {
        let game = app(
            "[Desktop Entry]\nType=Application\nName=Hades\nExec=moonlight\nX-Emrakul-Tv=game\n",
        );
        assert_eq!(game.tv_profile.as_deref(), Some("game"));
        let blank = app("[Desktop Entry]\nType=Application\nName=X\nExec=x\nX-Emrakul-Tv=\n");
        assert_eq!(blank.tv_profile, None);
    }

    #[test]
    fn a_malformed_brand_colour_is_ignored() {
        for bad in ["red", "#fff", "#12345g", "ff0033", "#ff00331", "#ff003é"] {
            let app = app(&format!(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nX-Emrakul-Brand={bad}\n"
            ));
            assert_eq!(app.brand, None, "{bad}");
            assert!(app.declared, "{bad}");
        }
    }

    #[test]
    fn only_the_desktop_entry_group_counts() {
        let app = app(
            "# comment\n[Desktop Entry]\nType=Application\nName=YouTube\nName[de]=Ytube\nExec=chromium\n\n[Desktop Action new]\nName=New\nExec=other\n",
        );
        assert_eq!(app.name, "YouTube");
        assert_eq!(app.exec, argv(&["chromium"]));
    }

    #[test]
    fn exec_quoting_and_field_codes() {
        let app = app(r#"[Desktop Entry]
Type=Application
Name=Web
Exec="/opt/my app/bin" --app=https://youtube.com/tv "a \"q\" \\\\ b" %U --name=%c 100%%
"#);
        assert_eq!(
            app.exec,
            argv(&[
                "/opt/my app/bin",
                "--app=https://youtube.com/tv",
                r#"a "q" \ b"#,
                "--name=Web",
                "100%",
            ])
        );
    }

    #[test]
    fn x_emrakul_quit_is_the_end_method() {
        let app = app(
            "[Desktop Entry]\nType=Application\nName=Hades\nExec=moonlight stream saturn Hades\nX-Emrakul-Quit=moonlight quit saturn\n",
        );
        assert_eq!(app.quit, Quit::Run(argv(&["moonlight", "quit", "saturn"])));
    }

    #[test]
    fn hidden_and_nodisplay_entries_are_not_apps() {
        for extra in [
            "NoDisplay=true",
            "Hidden=true",
            "OnlyShowIn=KDE;",
            "Type=Link",
        ] {
            // First of a duplicated key wins, so `extra` overrides the defaults.
            let text = format!("[Desktop Entry]\n{extra}\nType=Application\nName=X\nExec=x\n");
            assert_eq!(
                parse_entry(AppId::new("x.desktop"), &text),
                Entry::NotAnApp,
                "{extra}"
            );
        }
        assert!(matches!(
            parse_entry(
                AppId::new("x.desktop"),
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nNoDisplay=false\n"
            ),
            Entry::App(_)
        ));
    }

    /// A throwaway data dir tree, removed on drop.
    struct DataDirs(PathBuf);

    impl DataDirs {
        fn new(test: &str) -> Self {
            let root = std::env::temp_dir().join(format!("emrakul-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            Self(root)
        }

        fn write(&self, dir: &str, relative: &str, text: &str) {
            let path = self.0.join(dir).join("applications").join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        fn dirs(&self, names: &[&str]) -> Vec<PathBuf> {
            names.iter().map(|n| self.0.join(n)).collect()
        }
    }

    impl Drop for DataDirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry(name: &str, extra: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec=run-{name}\n{extra}\n")
    }

    #[test]
    fn discovers_across_data_dirs_and_earlier_dirs_win() {
        let tree = DataDirs::new("discover");
        tree.write("user", "kept.desktop", &entry("Mine", ""));
        tree.write("system", "kept.desktop", &entry("Theirs", ""));
        // A Hidden entry is how a user deletes a system-wide one.
        tree.write("user", "deleted.desktop", &entry("Deleted", "Hidden=true"));
        tree.write("system", "deleted.desktop", &entry("Deleted", ""));
        tree.write("system", "kde/konsole.desktop", &entry("Konsole", ""));
        tree.write("system", "notes.txt", "not an entry");
        tree.write("system", "broken.desktop", "garbage");

        let mut apps = discover(&tree.dirs(&["user", "missing", "system"]));
        apps.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        let found: Vec<_> = apps
            .iter()
            .map(|a| (a.id.as_str(), a.name.as_str()))
            .collect();
        assert_eq!(
            found,
            [("kde-konsole.desktop", "Konsole"), ("kept.desktop", "Mine")]
        );
    }

    #[test]
    fn entries_without_name_or_exec_are_not_apps() {
        for text in [
            "[Desktop Entry]\nType=Application\nExec=x\n",
            "[Desktop Entry]\nType=Application\nName=X\n",
            "[Desktop Entry]\nType=Application\nName=X\nExec=%U\n",
            "Type=Application\nName=X\nExec=x\n",
        ] {
            assert_eq!(
                parse_entry(AppId::new("x.desktop"), text),
                Entry::NotAnApp,
                "{text}"
            );
        }
    }
}
