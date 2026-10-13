//! Watching the chosen folders: a change anywhere in them asks the index helper to sync once they've settled
//! (`Settle`): two seconds for the folders (a camera import or an unzip is a burst of events); for code, a minute
//! without changes or ten minutes at most while they keep coming, as the Mac app's `Engine.codeChanged` has it.
//! `notify` uses ReadDirectoryChangesW on Windows, inotify on Linux, FSEvents on a Mac. Changes in DigUp's own index
//! folder are passed over, and in code folders so is what code search never reads: hidden folders (.git) and
//! dependencies or builds (`CODE_NOISE`).

use notify::event::{EventKind, ModifyKind};
use notify::{RecursiveMode, Watcher};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Folders whose changes don't concern the code index, because code search never goes into them: the engine's
/// `Rules.skippedFolderNames` (developer folders, then packages and libraries) and `Rules.codeSkippedFolderNames`, in
/// Sources/DigUpKit/Rules.swift. Keep them the same. (The Mac app's `Engine.codeNoise` is a part of them.)
const CODE_NOISE: &[&str] = &[
    // Rules.developerFolderNames
    "node_modules", "bower_components", "venv", "site-packages", "__pycache__", "DerivedData", "Pods", "Carthage",
    // Rules.libraryFolderNames
    "dist-packages", "jspm_packages", "elm-stuff", "Godeps", "vendor", "third_party", "third-party", "thirdparty",
    "3rdparty", "miniconda3", "anaconda3", "miniforge3", "mambaforge",
    // Rules.codeSkippedFolderNames
    "Vendor", "vendored", "external", "extern", "deps", "build", "Build", "dist", "out", "target", "obj", "coverage",
    "htmlcov", "generated", "__generated__", "wandb", "mlruns", "lightning_logs",
];

/// When a burst of changes is over: once nothing has changed for `quiet`, or, with `longest`, that long after its
/// first change even if changes keep coming.
#[derive(Clone, Copy, Debug)]
pub struct Settle {
    pub quiet: Duration,
    pub longest: Option<Duration>,
}

impl Settle {
    /// When a burst whose first change came at `first` and whose last came at `last` is over.
    fn deadline(&self, first: Instant, last: Instant) -> Instant {
        let quiet = last + self.quiet;
        match self.longest.and_then(|longest| first.checked_add(longest)) {
            Some(cap) => quiet.min(cap),
            None => quiet,
        }
    }
}

/// What the watcher's callback tells its thread.
enum Message {
    /// Something that counts changed.
    Changed,
    /// A folder appeared in a code folder: it is watched too (Linux, where code folders are watched one by one).
    Folder(PathBuf),
}

pub struct FolderWatcher {
    _watcher: Option<Arc<Mutex<notify::RecommendedWatcher>>>,
}

impl FolderWatcher {
    /// Watches `folders`; `changed` runs on the watcher's own thread once a burst of changes has settled. With
    /// `code`, only changes code search reads count. Dropping it stops.
    pub fn start(folders: &[PathBuf], ignore: Option<PathBuf>, settle: Settle, code: bool,
                 changed: impl Fn() + Send + 'static) -> FolderWatcher {
        // inotify watches every folder on its own, and a tree of repos holds tens of thousands of them, most of them
        // dependencies and builds, against a limit of 65,536 for all programs (max_user_watches). So on Linux code
        // folders are walked here, leaving those out, and folders that appear later are added as they come. Elsewhere
        // one watch covers a whole tree.
        let one_by_one = code && cfg!(target_os = "linux");
        let (sender, receiver) = mpsc::channel::<Message>();
        let roots = folders.to_vec();
        let skipped = ignore.clone();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else { return };
            if event.kind.is_access() {
                return;
            }
            let ours = |path: &PathBuf| skipped.as_ref().is_some_and(|folder| path.starts_with(folder));
            let counts = |path: &PathBuf| !ours(path) && !(code && is_code_noise(path, &roots));
            if one_by_one && matches!(event.kind, EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))) {
                for path in event.paths.iter().filter(|path| counts(path) && path.is_dir()) {
                    let _ = sender.send(Message::Folder(path.clone()));
                }
            }
            if event.paths.is_empty() || event.paths.iter().any(counts) {
                let _ = sender.send(Message::Changed);
            }
        });
        let watcher = match watcher {
            Ok(watcher) => Arc::new(Mutex::new(watcher)),
            Err(error) => {
                eprintln!("digup-shell: can't watch folders: {error}");
                return FolderWatcher { _watcher: None };
            }
        };
        for folder in folders {
            if one_by_one {
                watch_one_by_one(&watcher, &code_folders_to_watch(folder, ignore.as_deref()));
            } else if let Err(error) = watcher.lock().unwrap().watch(folder, RecursiveMode::Recursive) {
                eprintln!("digup-shell: can't watch {}: {error}", folder.display());
            }
        }
        // The thread holds the watcher weakly: dropping the FolderWatcher drops it, which ends the thread.
        let weak = Arc::downgrade(&watcher);
        let ignore = ignore.clone();
        std::thread::spawn(move || {
            let mut add = |folder: PathBuf| {
                if let Some(watcher) = weak.upgrade() {
                    watch_one_by_one(&watcher, &code_folders_to_watch(&folder, ignore.as_deref()));
                }
            };
            while let Ok(message) = receiver.recv() {
                match message {
                    Message::Folder(folder) => add(folder),
                    Message::Changed => {
                        if !wait_until_settled(&receiver, &settle, &mut add) {
                            return;
                        }
                        changed();
                    }
                }
            }
        });
        FolderWatcher { _watcher: Some(watcher) }
    }
}

/// Waits out a burst whose first change has just come: true once it's over, false when the watcher went away.
/// Folders that appear meanwhile go to `add`.
fn wait_until_settled(receiver: &Receiver<Message>, settle: &Settle, add: &mut dyn FnMut(PathBuf)) -> bool {
    let first = Instant::now();
    let mut last = first;
    loop {
        match receiver.recv_timeout(settle.deadline(first, last).saturating_duration_since(Instant::now())) {
            Ok(Message::Changed) => last = Instant::now(),
            Ok(Message::Folder(folder)) => add(folder),
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// Watches each of `folders` (not what's in them), saying once if the system's limit stops it.
fn watch_one_by_one(watcher: &Mutex<notify::RecommendedWatcher>, folders: &[PathBuf]) {
    let mut watcher = watcher.lock().unwrap();
    let failed = folders.iter().filter_map(|folder| watcher.watch(folder, RecursiveMode::NonRecursive).err()).count();
    if failed > 0 {
        eprintln!("digup-shell: can't watch {failed} of {} code folders (fs.inotify.max_user_watches?)", folders.len());
    }
}

/// A path code search never reads: hidden (.git, .venv, .DS_Store) or under one of `CODE_NOISE`, counted from the
/// code folder it's in (a code folder may itself be in one).
fn is_code_noise(path: &Path, roots: &[PathBuf]) -> bool {
    let inside = roots.iter().filter_map(|root| path.strip_prefix(root).ok()).min_by_key(|rest| rest.as_os_str().len());
    inside.unwrap_or(path).components().any(|part| match part {
        Component::Normal(name) => is_noise_name(&name.to_string_lossy()),
        _ => false,
    })
}

fn is_noise_name(name: &str) -> bool {
    name.starts_with('.') || CODE_NOISE.contains(&name)
}

/// Installed software, whatever the folder is called, told by what it holds: the engine's
/// `Rules.holdsInstalledPackages` (a virtualenv, a conda environment, a tagged cache, Go's module cache). A few stats,
/// made once per folder as it's walked; changes are filtered by name only, since inside such a folder nothing is
/// watched to begin with.
fn holds_installed_packages(folder: &Path) -> bool {
    let holds = |name: &str| folder.join(name).exists();
    if holds("pyvenv.cfg") || holds("CACHEDIR.TAG") || folder.join("conda-meta").is_dir() {
        return true;
    }
    let named = |path: Option<&Path>, name: &str| path.and_then(Path::file_name).is_some_and(|found| found == name);
    named(Some(folder), "mod") && named(folder.parent(), "pkg") && folder.join("cache/download").is_dir()
}

/// `folder` and the folders under it that code search reads, without following links, and leaving out `ignore`
/// (DigUp's index) and installed packages.
fn code_folders_to_watch(folder: &Path, ignore: Option<&Path>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut left = vec![folder.to_path_buf()];
    while let Some(folder) = left.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        found.push(folder);
        for entry in entries.flatten() {
            let path = entry.path();
            // file_type doesn't follow links, so a link to a folder isn't walked (inotify doesn't follow them either).
            let is_folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
            let noise = is_noise_name(&entry.file_name().to_string_lossy());
            let index = ignore.is_some_and(|ignore| path == ignore);
            if is_folder && !noise && !index && !holds_installed_packages(&path) {
                left.push(path);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn code_search_never_reads_hidden_folders_dependencies_or_builds() {
        let roots = [PathBuf::from("/home/me/src")];
        for noise in ["/home/me/src/app/node_modules/left-pad/index.js", "/home/me/src/app/.git/index",
                      "/home/me/src/tool/target/debug/tool", "/home/me/src/app/.venv/bin/python",
                      "/home/me/src/web/dist/bundle.js", "/home/me/src/app/__pycache__/x.pyc"] {
            assert!(is_code_noise(Path::new(noise), &roots), "{noise}");
        }
        for code in ["/home/me/src/app/main.py", "/home/me/src/tool/src/lib.rs", "/home/me/src/app/README.md",
                     "/home/me/src/app/vendor-notes/x.md"] {
            assert!(!is_code_noise(Path::new(code), &roots), "{code}");
        }
        // The engine's own lists (Rules.swift): libraries and installed packages leave both indexes.
        for library in ["dist-packages/x.py", "third-party/x.c", "miniconda3/lib/x.py", "Godeps/x.go", "external/x.c"] {
            assert!(is_code_noise(&Path::new("/home/me/src/app").join(library), &roots), "{library}");
        }
        // Hidden files count as noise too, as in the Mac app: the crawler never reads them.
        assert!(is_code_noise(Path::new("/home/me/src/app/.gitignore"), &roots));
        // Only what's under the code folder counts: a code folder may itself sit in a hidden or "build" folder.
        let roots = [PathBuf::from("/home/me/.local/build/src")];
        assert!(!is_code_noise(Path::new("/home/me/.local/build/src/main.c"), &roots));
        assert!(is_code_noise(Path::new("/home/me/.local/build/src/.git/HEAD"), &roots));
    }

    #[test]
    fn code_folders_are_watched_without_the_noise() {
        let root = std::env::temp_dir().join(format!("digup-watch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for folder in ["app/src/deep", "app/node_modules/pkg", "app/.git/objects", "tool/target/debug", "tool/src",
                       "index/previews"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        std::fs::write(root.join("app/src/main.py"), "print()").unwrap();
        let mut watched = code_folders_to_watch(&root, Some(&root.join("index")));
        watched.sort();
        let names: Vec<String> = watched.iter().map(|path| {
            path.strip_prefix(&root).unwrap().to_string_lossy().into_owned()
        }).collect();
        assert_eq!(names, ["", "app", "app/src", "app/src/deep", "tool", "tool/src"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn installed_packages_are_not_watched_whatever_their_name() {
        let root = std::env::temp_dir().join(format!("digup-watch-packages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for folder in ["app/src", "app/env/lib", "app/ml/conda-meta", "app/cache/data", "go/pkg/mod/cache/download",
                       "app/mod", "app/notes"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        std::fs::write(root.join("app/env/pyvenv.cfg"), "home = /usr/bin").unwrap();
        std::fs::write(root.join("app/cache/CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55").unwrap();
        std::fs::write(root.join("app/notes/conda-meta"), "a file, not a conda env").unwrap();
        let mut watched = code_folders_to_watch(&root, None);
        watched.sort();
        let names: Vec<String> = watched.iter().map(|path| {
            path.strip_prefix(&root).unwrap().to_string_lossy().into_owned()
        }).collect();
        assert_eq!(names, ["", "app", "app/mod", "app/notes", "app/src", "go", "go/pkg"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_burst_settles_once_quiet() {
        let settle = Settle { quiet: Duration::from_millis(60), longest: None };
        let started = Instant::now();
        assert_eq!(settle.deadline(started, started), started + Duration::from_millis(60));
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            for _ in 0..5 {
                std::thread::sleep(Duration::from_millis(20));
                let _ = sender.send(Message::Changed);
            }
            std::thread::sleep(Duration::from_secs(2));   // the watcher is still there, but quiet
        });
        assert!(wait_until_settled(&receiver, &settle, &mut |_| {}));
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(150) && took < Duration::from_millis(1000), "{took:?}");
    }

    #[test]
    fn a_folder_that_never_goes_quiet_still_syncs_at_the_longest_wait() {
        let settle = Settle { quiet: Duration::from_millis(200), longest: Some(Duration::from_millis(300)) };
        let first = Instant::now();
        assert_eq!(settle.deadline(first, first + Duration::from_millis(250)), first + Duration::from_millis(300));
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            // A change every 50 ms for two seconds: never quiet for 200 ms.
            for _ in 0..40 {
                if sender.send(Message::Changed).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let started = Instant::now();
        assert!(wait_until_settled(&receiver, &settle, &mut |_| {}));
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(290) && took < Duration::from_millis(700), "{took:?}");
    }

    #[test]
    fn a_watcher_gone_mid_burst_is_not_a_change() {
        let settle = Settle { quiet: Duration::from_secs(5), longest: None };
        let (sender, receiver) = channel::<Message>();
        drop(sender);
        assert!(!wait_until_settled(&receiver, &settle, &mut |_| {}));
    }
}
