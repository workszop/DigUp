//! What the person chose: the folders DigUp searches, what it skips in them, code search's folders, the hotkey, and
//! whether setup is done. A JSON file in the app's config folder (%APPDATA%\com.abdurrahim.DigUp on Windows,
//! ~/.config/com.abdurrahim.DigUp on Linux). Launch arguments (`--roots`, `--hotkey`) stand in for it in dev runs.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// The folders searched and kept indexed.
    pub folders: Vec<String>,
    /// Folders inside them that aren't (`FolderSelection` in the engine).
    pub skipped: Vec<String>,
    /// File extensions left out ("heic", "txt").
    pub skip_types: Vec<String>,
    /// The panel's hotkey (Tauri's accelerator form); empty: the default.
    pub hotkey: String,
    /// The program code results open in ("zed", "code", "idea"...); empty: the first one installed.
    pub editor: String,
    /// Code search's folders, whose code goes into the code index; none: code search is off. Searched with "code:"
    /// only (the Mac app's `codeRoots`).
    pub code_roots: Vec<String>,
    /// Folders inside them that aren't (the Mac app's `codeExcluded`).
    pub code_skipped: Vec<String>,
    /// Onboarding is done: the shell starts in the tray.
    pub set_up: bool,
}

impl Settings {
    /// The file's settings, or the defaults when there's none (or it can't be read).
    pub fn load(path: &Path) -> Settings {
        std::fs::read(path).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default()
    }

    /// Written whole to a temporary file first, so a crash never leaves half of it.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let temporary = path.with_extension("json.new");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temporary, path)
    }

    /// The code folders from now on (none turns code search off), as the Mac app's `applyCodeFolders` keeps them
    /// (`FolderSelection(chosen:)`): each written one way, once, and none inside another that already searches it
    /// whole. A skipped folder goes with the code folder it was in, as `removeCodeFolder` has it.
    pub fn set_code_folders(&mut self, roots: Vec<String>) {
        let roots = unique(roots.iter().map(|root| folder_path(root)));
        let skipped = unique(self.code_skipped.iter().map(|skip| folder_path(skip)));
        let searched_whole_by = |folder: &str, other: &str| {
            let skipped_there = |skip: &String| is_inside(skip, other) && (folder == skip || is_inside(folder, skip));
            is_inside(folder, other) && !skipped.iter().any(skipped_there)
        };
        let kept: Vec<String> = roots.iter()
            .filter(|root| !roots.iter().any(|other| searched_whole_by(root, other)))
            .cloned()
            .collect();
        self.code_skipped = skipped.into_iter().filter(|skip| kept.iter().any(|root| is_inside(skip, root))).collect();
        self.code_roots = kept;
    }
}

// MARK: Folder paths

/// A folder as the settings keep it: "~" for the home folder expanded, and no separator at its end (the Mac app's
/// `AppSettings.folderURL`, links left as they are).
pub fn folder_path(path: &str) -> String {
    let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok();
    folder_path_in(path, home.as_deref())
}

fn folder_path_in(path: &str, home: Option<&str>) -> String {
    let path = path.trim();
    let path = match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => format!("{home}{rest}"),
        _ => path.to_string(),
    };
    let trimmed = path.trim_end_matches(['/', '\\']);
    // The root of a drive or of everything keeps its separator ("/", "C:\").
    if trimmed.is_empty() || trimmed.ends_with(':') {
        return path[..(trimmed.len() + 1).min(path.len())].to_string();
    }
    trimmed.to_string()
}

/// `folder` is somewhere under `other` (not `other` itself).
fn is_inside(folder: &str, other: &str) -> bool {
    folder != other && Path::new(folder).starts_with(other)
}

fn unique(paths: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = Vec::new();
    for path in paths {
        if !seen.contains(&path) {
            seen.push(path);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_file(name: &str) -> std::path::PathBuf {
        let folder = std::env::temp_dir().join(format!("digup-settings-test-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        folder.join(name)
    }

    #[test]
    fn code_folders_are_saved_and_read_back() {
        let path = scratch_file("code.json");
        let settings = Settings {
            folders: vec!["/home/me/Documents".into()],
            code_roots: vec!["/home/me/src".into(), "/home/me/work".into()],
            code_skipped: vec!["/home/me/src/vendor".into()],
            set_up: true,
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        // The file names them as the rest of it is named.
        let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["codeRoots"], serde_json::json!(["/home/me/src", "/home/me/work"]));
        assert_eq!(written["codeSkipped"], serde_json::json!(["/home/me/src/vendor"]));
        let read = Settings::load(&path);
        assert_eq!(read.code_roots, settings.code_roots);
        assert_eq!(read.code_skipped, settings.code_skipped);
        assert_eq!(read.folders, settings.folders);
        assert!(read.set_up);
    }

    #[test]
    fn settings_from_before_code_search_still_load() {
        let path = scratch_file("old.json");
        std::fs::write(&path, r#"{"folders":["/home/me/Pictures"],"skipped":["/home/me/Pictures/raw"],
            "skipTypes":["heic"],"hotkey":"","editor":"code","setUp":true}"#).unwrap();
        let read = Settings::load(&path);
        assert_eq!(read.folders, ["/home/me/Pictures"]);
        assert_eq!(read.skip_types, ["heic"]);
        assert!(read.set_up);
        assert!(read.code_roots.is_empty());
        assert!(read.code_skipped.is_empty());
    }

    #[test]
    fn a_code_folder_taken_out_takes_its_skipped_folders_with_it() {
        let mut settings = Settings {
            code_roots: vec!["/src".into(), "/work".into()],
            code_skipped: vec!["/src/vendor".into(), "/work/old".into(), "/srcfiles/x".into()],
            ..Settings::default()
        };
        settings.set_code_folders(vec!["/work".into()]);
        assert_eq!(settings.code_roots, ["/work"]);
        assert_eq!(settings.code_skipped, ["/work/old"]);
        settings.set_code_folders(Vec::new());
        assert!(settings.code_roots.is_empty());
        assert!(settings.code_skipped.is_empty());
    }

    #[test]
    fn folders_are_kept_without_tilde_or_trailing_separator() {
        let home = Some("/home/me");
        assert_eq!(folder_path_in("~/src/", home), "/home/me/src");
        assert_eq!(folder_path_in("  ~  ", home), "/home/me");
        assert_eq!(folder_path_in("/work//", home), "/work");
        assert_eq!(folder_path_in("/", home), "/");
        assert_eq!(folder_path_in("~other/x", home), "~other/x");   // another user's home isn't looked up
        assert_eq!(folder_path_in("C:\\Code\\", None), "C:\\Code");
        assert_eq!(folder_path_in("C:\\", None), "C:\\");
    }

    #[test]
    fn a_code_folder_inside_another_goes_unless_it_is_skipped_there() {
        let mut settings = Settings {
            code_skipped: vec!["/src/vendor/".into()],
            ..Settings::default()
        };
        settings.set_code_folders(vec!["/src/".into(), "/src/app".into(), "/src/vendor/lib".into(), "/src".into(),
                                       "/work".into()]);
        assert_eq!(settings.code_roots, ["/src", "/src/vendor/lib", "/work"]);
        assert_eq!(settings.code_skipped, ["/src/vendor"]);
    }
}
