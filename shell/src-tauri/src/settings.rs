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

    /// The code folders from now on (none turns code search off). A skipped folder goes with the code folder it was
    /// in, as `removeCodeFolder` has it in the Mac app.
    pub fn set_code_folders(&mut self, roots: Vec<String>) {
        self.code_skipped.retain(|skip| roots.iter().any(|root| Path::new(skip).starts_with(root)));
        self.code_roots = roots;
    }
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
}
