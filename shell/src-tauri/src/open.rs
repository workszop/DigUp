//! Opening a result where it matched, on Linux: a recording at its moment, a PDF at its page, code at its line (the Mac
//! app's `Opener` and `CodeEditor`). Each place needs a program that can be told it; without one, the file opens in
//! its default app, at its start. Arguments go to the program as they are, never through a shell.

use serde::Deserialize;

/// What the panel knows of a result (a hit from `digup search-service`).
#[derive(Clone, Debug, Deserialize)]
pub struct Target {
    pub path: String,
    pub kind: String,
    #[serde(default)]
    pub segment: Option<String>,
    #[serde(default)]
    pub loc: Option<f64>,
    #[serde(default)]
    pub focus: Option<f64>,
}

/// Editors that can be told a line, in the order one is picked when none is chosen in the settings.
const EDITORS: &[&str] = &["zed", "code", "cursor", "codium", "windsurf", "subl", "idea", "pycharm", "webstorm",
                           "clion", "goland", "rustrover", "phpstorm", "rubymine"];

impl Target {
    /// The PDF page that matched (1-based).
    fn page(&self) -> Option<u32> {
        let on_page = matches!(self.segment.as_deref(), Some("page" | "page_image"));
        (self.kind == "pdf" && on_page).then_some(self.loc).flatten().map(|loc| loc.max(1.0) as u32)
    }

    /// Seconds into a recording where the match starts.
    fn moment(&self) -> Option<f64> {
        let heard_or_seen = matches!(self.segment.as_deref(), Some("frame" | "audio"));
        (matches!(self.kind.as_str(), "video" | "audio") && heard_or_seen).then_some(self.loc).flatten()
    }

    /// The line code opens at: where the name is defined or the words are, else where its stretch starts (a
    /// notebook's at its top: editors can't be told a cell).
    fn line(&self) -> u32 {
        if self.segment.as_deref() != Some("lines") {
            return 1;
        }
        self.focus.or(self.loc).unwrap_or(1.0).max(1.0) as u32
    }
}

/// The program and arguments that open `target` at its place, given the editor chosen for code ("" for the first one
/// installed) and `has` (is this program installed). None: the default app opens it, at its start.
pub fn command(target: &Target, editor: &str, has: impl Fn(&str) -> bool) -> Option<Vec<String>> {
    let path = target.path.clone();
    if target.kind == "code" {
        let editor = if editor.is_empty() { EDITORS.iter().copied().find(|name| has(name))? } else { has(editor).then_some(editor)? };
        let line = target.line();
        return Some(match editor {
            "zed" | "subl" => vec![editor.into(), format!("{path}:{line}")],
            "code" | "cursor" | "codium" | "windsurf" => vec![editor.into(), "-g".into(), format!("{path}:{line}")],
            _ => vec![editor.into(), "--line".into(), line.to_string(), path], // JetBrains launchers
        });
    }
    if let Some(moment) = target.moment().filter(|moment| *moment >= 1.0) {
        if has("mpv") {
            return Some(vec!["mpv".into(), format!("--start={moment:.1}"), "--".into(), path]);
        }
        if has("vlc") {
            return Some(vec!["vlc".into(), format!("--start-time={moment:.1}"), path]);
        }
        return None;
    }
    if let Some(page) = target.page().filter(|page| *page > 1) {
        if has("okular") {
            return Some(vec!["okular".into(), "--page".into(), page.to_string(), path]);
        }
        for viewer in ["evince", "papers"] {
            if has(viewer) {
                return Some(vec![viewer.into(), format!("--page-index={page}"), path]);
            }
        }
    }
    None
}

/// Whether a program is on PATH.
pub fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|folder| folder.join(name).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(kind: &str, segment: &str, loc: Option<f64>, focus: Option<f64>, path: &str) -> Target {
        Target { path: path.into(), kind: kind.into(), segment: Some(segment.into()), loc, focus }
    }
    fn only<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |name| names.contains(&name)
    }
    fn strings(parts: &[&str]) -> Option<Vec<String>> {
        Some(parts.iter().map(|part| part.to_string()).collect())
    }

    #[test]
    fn video_opens_at_its_moment_in_mpv_then_vlc() {
        let video = target("video", "frame", Some(6.0), None, "/v/zoo clip.ts");
        assert_eq!(command(&video, "", only(&["mpv", "vlc"])), strings(&["mpv", "--start=6.0", "--", "/v/zoo clip.ts"]));
        assert_eq!(command(&video, "", only(&["vlc"])), strings(&["vlc", "--start-time=6.0", "/v/zoo clip.ts"]));
    }

    #[test]
    fn without_a_player_or_moment_it_is_the_default_app() {
        assert_eq!(command(&target("video", "frame", Some(6.0), None, "/v/a.mp4"), "", only(&[])), None);
        assert_eq!(command(&target("audio", "audio", Some(0.4), None, "/a.wav"), "", only(&["mpv"])), None);
        assert_eq!(command(&target("image", "image", None, None, "/p.jpg"), "", only(&["mpv", "evince"])), None);
    }

    #[test]
    fn pdf_opens_at_its_page() {
        let pdf = target("pdf", "page", Some(3.0), None, "/d/lease agreement.pdf");
        assert_eq!(command(&pdf, "", only(&["evince"])), strings(&["evince", "--page-index=3", "/d/lease agreement.pdf"]));
        assert_eq!(command(&pdf, "", only(&["okular", "evince"])), strings(&["okular", "--page", "3", "/d/lease agreement.pdf"]));
        assert_eq!(command(&target("pdf", "page", Some(1.0), None, "/d/x.pdf"), "", only(&["evince"])), None);
    }

    #[test]
    fn code_opens_at_its_line_in_the_chosen_or_first_editor() {
        let code = target("code", "lines", Some(10.0), Some(12.0), "/r/src/main.rs");
        assert_eq!(command(&code, "", only(&["code", "zed"])), strings(&["zed", "/r/src/main.rs:12"]));
        assert_eq!(command(&code, "code", only(&["code", "zed"])), strings(&["code", "-g", "/r/src/main.rs:12"]));
        assert_eq!(command(&code, "idea", only(&["idea"])), strings(&["idea", "--line", "12", "/r/src/main.rs"]));
        assert_eq!(command(&code, "nano-not-here", only(&["zed"])), None);
        let cell = target("code", "cells", Some(2.0), None, "/r/n.ipynb");
        assert_eq!(command(&cell, "", only(&["zed"])), strings(&["zed", "/r/n.ipynb:1"]));
    }

    #[test]
    fn odd_names_pass_through_untouched() {
        let name = "/m/-ইলিশ: café 日本.mp4";
        let video = target("video", "audio", Some(25.0), None, name);
        assert_eq!(command(&video, "", only(&["mpv"])), strings(&["mpv", "--start=25.0", "--", name]));
    }
}
