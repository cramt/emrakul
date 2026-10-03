//! Home's order: most recently launched first, kept across restarts in
//! `$XDG_STATE_HOME/emrakul/recent`, one desktop file ID per line.

use std::path::PathBuf;

use anyhow::Context;

use crate::apps::{App, AppId, env_dir};

pub struct Recency {
    path: PathBuf,
    /// Most recent first, no duplicates.
    ids: Vec<AppId>,
}

impl Recency {
    pub fn default_path() -> anyhow::Result<PathBuf> {
        let state = env_dir("XDG_STATE_HOME")
            .or_else(|| Some(env_dir("HOME")?.join(".local/state")))
            .context("neither XDG_STATE_HOME nor HOME is set to an absolute path")?;
        Ok(state.join("emrakul/recent"))
    }

    /// A missing file is a fresh start, not an error.
    pub fn load(path: PathBuf) -> anyhow::Result<Self> {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let mut ids: Vec<AppId> = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let id = AppId::new(line);
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(Self { path, ids })
    }

    /// Marks `id` as just launched and writes the file.
    pub fn touch(&mut self, id: &AppId) -> anyhow::Result<()> {
        self.ids.retain(|known| known != id);
        self.ids.insert(0, id.clone());
        self.save()
            .with_context(|| format!("writing {}", self.path.display()))
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text = String::new();
        for id in &self.ids {
            text.push_str(id.as_str());
            text.push('\n');
        }
        // Written aside and renamed over, so a crash mid-write can't leave
        // Home with a truncated order.
        let partial = self.path.with_extension("partial");
        std::fs::write(&partial, text)?;
        std::fs::rename(partial, &self.path)
    }

    /// Recently launched apps first, then the never-launched ones: those
    /// nixconf declared for the TV, then the rest, each by name.
    pub fn order(&self, mut apps: Vec<App>) -> Vec<App> {
        apps.sort_by_cached_key(|app| {
            let rank = self.ids.iter().position(|id| *id == app.id);
            (rank.is_none(), rank, !app.declared, app.name.to_lowercase())
        });
        apps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::{Argv, Quit};

    fn app(id: &str, name: &str) -> App {
        declared_app(id, name, false)
    }

    fn declared_app(id: &str, name: &str, declared: bool) -> App {
        App {
            id: AppId::new(id),
            name: name.into(),
            exec: Argv {
                program: "true".into(),
                args: vec![],
            },
            quit: Quit::Close,
            icon: None,
            declared,
            brand: None,
            tv_profile: None,
            back: Default::default(),
            pad_reader: crate::gamepad::PadReader::Emrakul,
        }
    }

    fn names(apps: &[App]) -> Vec<&str> {
        apps.iter().map(|a| a.name.as_str()).collect()
    }

    struct StateFile(PathBuf);

    impl StateFile {
        fn new(test: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("emrakul-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir.join("emrakul/recent"))
        }
    }

    impl Drop for StateFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap().parent().unwrap());
        }
    }

    fn apps() -> Vec<App> {
        vec![
            app("yt.desktop", "YouTube"),
            app("jf.desktop", "jellyfin"),
            app("hades.desktop", "Hades"),
            app("celeste.desktop", "Celeste"),
        ]
    }

    #[test]
    fn nothing_launched_yet_sorts_by_name() {
        let state = StateFile::new("fresh");
        let recency = Recency::load(state.0.clone()).unwrap();
        assert_eq!(
            names(&recency.order(apps())),
            ["Celeste", "Hades", "jellyfin", "YouTube"]
        );
    }

    #[test]
    fn declared_entries_come_before_the_rest_until_used() {
        let state = StateFile::new("declared");
        let mut recency = Recency::load(state.0.clone()).unwrap();
        let mut apps = apps();
        apps.push(declared_app("zz.desktop", "Zelda", true));
        apps.push(declared_app("ark.desktop", "Ark", false));
        apps.push(declared_app("bal.desktop", "Balatro", true));
        assert_eq!(
            names(&recency.order(apps.clone())),
            [
                "Balatro", "Zelda", "Ark", "Celeste", "Hades", "jellyfin", "YouTube"
            ]
        );
        recency.touch(&AppId::new("hades.desktop")).unwrap();
        assert_eq!(
            names(&recency.order(apps)),
            [
                "Hades", "Balatro", "Zelda", "Ark", "Celeste", "jellyfin", "YouTube"
            ]
        );
    }

    #[test]
    fn launched_apps_come_first_newest_first_and_survive_a_restart() {
        let state = StateFile::new("restart");
        let mut recency = Recency::load(state.0.clone()).unwrap();
        recency.touch(&AppId::new("yt.desktop")).unwrap();
        recency.touch(&AppId::new("hades.desktop")).unwrap();
        recency.touch(&AppId::new("yt.desktop")).unwrap();
        assert_eq!(
            names(&recency.order(apps())),
            ["YouTube", "Hades", "Celeste", "jellyfin"]
        );

        let reloaded = Recency::load(state.0.clone()).unwrap();
        assert_eq!(
            names(&reloaded.order(apps())),
            ["YouTube", "Hades", "Celeste", "jellyfin"]
        );
    }

    #[test]
    fn uninstalled_apps_in_the_file_are_harmless() {
        let state = StateFile::new("uninstalled");
        std::fs::create_dir_all(state.0.parent().unwrap()).unwrap();
        std::fs::write(&state.0, "gone.desktop\n\nceleste.desktop\ngone.desktop\n").unwrap();
        let recency = Recency::load(state.0.clone()).unwrap();
        assert_eq!(
            names(&recency.order(apps())),
            ["Celeste", "Hades", "jellyfin", "YouTube"]
        );
    }
}
