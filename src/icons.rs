//! Finding an app's `Icon=` on disk, in the hicolor theme every icon theme
//! falls back to (<https://specifications.freedesktop.org/icon-theme-spec/latest/>).
//! ganymede has no desktop to pick another theme, so hicolor is the theme.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconFile {
    Svg(PathBuf),
    Png(PathBuf),
}

impl IconFile {
    /// By extension. XPM and anything else gets the letter instead.
    fn at(path: PathBuf) -> Option<Self> {
        if !path.is_file() {
            return None;
        }
        match path.extension()?.to_str()? {
            "svg" => Some(Self::Svg(path)),
            "png" => Some(Self::Png(path)),
            _ => None,
        }
    }
}

/// A tile shows its icon at about 300 px, so a scalable icon is best, then
/// the largest PNG, wherever they are installed.
pub fn find(icon: &str, data_dirs: &[PathBuf]) -> Option<IconFile> {
    if Path::new(icon).is_absolute() {
        return IconFile::at(icon.into());
    }
    let hicolor = |dir: &PathBuf| dir.join("icons/hicolor");
    let scalable = data_dirs
        .iter()
        .find_map(|dir| IconFile::at(hicolor(dir).join(format!("scalable/apps/{icon}.svg"))));
    let largest_png = || {
        data_dirs
            .iter()
            .flat_map(|dir| {
                std::fs::read_dir(hicolor(dir))
                    .into_iter()
                    .flatten()
                    .flatten()
            })
            .filter_map(|size_dir| {
                let name = size_dir.file_name();
                let (width, _) = name.to_str()?.split_once('x')?;
                let size: u32 = width.parse().ok()?;
                let file = IconFile::at(size_dir.path().join(format!("apps/{icon}.png")))?;
                Some((size, file))
            })
            // Not max_by_key: that keeps the last of equals, and the earlier
            // data dir should win a tie.
            .fold(None, |best, (size, file)| match best {
                Some((best_size, _)) if best_size >= size => best,
                _ => Some((size, file)),
            })
            .map(|(_, file)| file)
    };
    let pixmap = || {
        data_dirs.iter().find_map(|dir| {
            ["svg", "png"]
                .into_iter()
                .find_map(|ext| IconFile::at(dir.join(format!("pixmaps/{icon}.{ext}"))))
        })
    };
    scalable.or_else(largest_png).or_else(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);

    impl Tree {
        fn new(test: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("emrakul-icons-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            Self(root)
        }

        fn touch(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
            path
        }

        fn dirs(&self) -> Vec<PathBuf> {
            vec![
                self.0.join("user"),
                self.0.join("missing"),
                self.0.join("system"),
            ]
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_largest_png_in_any_data_dir_wins() {
        let tree = Tree::new("png");
        tree.touch("user/icons/hicolor/48x48/apps/steam.png");
        let big = tree.touch("system/icons/hicolor/256x256/apps/steam.png");
        tree.touch("system/icons/hicolor/32x32/apps/steam.png");
        tree.touch("system/pixmaps/steam.png");
        assert_eq!(find("steam", &tree.dirs()), Some(IconFile::Png(big)));
    }

    #[test]
    fn a_scalable_icon_beats_any_png() {
        let tree = Tree::new("svg");
        tree.touch("user/icons/hicolor/512x512/apps/kate.png");
        let svg = tree.touch("system/icons/hicolor/scalable/apps/kate.svg");
        assert_eq!(find("kate", &tree.dirs()), Some(IconFile::Svg(svg)));
    }

    #[test]
    fn pixmaps_are_the_last_resort() {
        let tree = Tree::new("pixmaps");
        let pixmap = tree.touch("system/pixmaps/xterm.png");
        tree.touch("system/pixmaps/old.xpm");
        assert_eq!(find("xterm", &tree.dirs()), Some(IconFile::Png(pixmap)));
        assert_eq!(find("old", &tree.dirs()), None);
        assert_eq!(find("nothing", &tree.dirs()), None);
    }

    #[test]
    fn an_absolute_path_is_used_as_is() {
        let tree = Tree::new("absolute");
        let svg = tree.touch("art/youtube.svg");
        assert_eq!(
            find(svg.to_str().unwrap(), &tree.dirs()),
            Some(IconFile::Svg(svg))
        );
        assert_eq!(find("/nonexistent/icon.png", &tree.dirs()), None);
    }
}
