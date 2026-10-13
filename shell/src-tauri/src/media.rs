//! Sound and video for the panel's previews, on Linux. WebKitGTK plays media through GStreamer and hands it the media's
//! URL as it is: it lets through only http(s), file, blob and data URLs, and GStreamer has no source for Tauri's
//! `asset://` either, so an `<audio>` or `<video>` given a `convertFileSrc` URL fails at once (MEDIA_ERR_SRC_NOT_SUPPORTED)
//! while pictures from the same folders show. The previews' media comes from here instead: a server on the loopback
//! whose address carries a random key only the panel's page is told, serving only what the asset protocol's scope
//! allows, in the byte ranges a player asks for.

use http_range::{HttpRange, HttpRangeParseError};
use percent_encoding::percent_decode_str;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// A request's head is a line and a few headers; anything longer isn't from the panel.
const MAX_HEAD: u64 = 16 * 1024;
/// How long a connection may take to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Starts the server for the files `scope` allows; the URL of a file is the address this returns followed by its
/// path, percent-encoded as one component (`encodeURIComponent`).
pub fn serve(scope: tauri::scope::fs::Scope) -> io::Result<String> {
    serve_where(move |path| scope.is_allowed(path))
}

/// `serve`, with `allowed` saying which files may be read.
fn serve_where(allowed: impl Fn(&Path) -> bool + Send + Sync + 'static) -> io::Result<String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let key = new_key()?;
    let address = format!("http://127.0.0.1:{}/{key}/", listener.local_addr()?.port());
    let allowed: Arc<dyn Fn(&Path) -> bool + Send + Sync> = Arc::new(allowed);
    thread::Builder::new().name("media".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let (key, allowed) = (key.clone(), allowed.clone());
            // A player reads a stream for as long as it plays, and asks again for each seek: one thread a connection.
            let _ = thread::Builder::new().name("media-reply".into()).spawn(move || {
                let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
                let _ = answer(&mut BufReader::new(&stream), &key, &*allowed, &mut &stream);
            });
        }
    })?;
    Ok(address)
}

/// 128 random bits as hex: no other program or page can guess the server's addresses.
fn new_key() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

// MARK: One request

/// Reads a request from `request` and writes its answer to `out`: the file (or the range of it asked for) when the key
/// is right and `allowed` lets the file be read.
fn answer(request: &mut impl BufRead, key: &str, allowed: &dyn Fn(&Path) -> bool, out: &mut impl Write) -> io::Result<()> {
    let mut lines = Vec::new();
    let mut head = request.take(MAX_HEAD);
    loop {
        let mut line = String::new();
        if head.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let Some(first) = lines.first() else { return Ok(()) };
    let mut words = first.split(' ');
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let only_head = method == "HEAD";
    if method != "GET" && !only_head {
        return status(out, "405 Method Not Allowed", "");
    }
    let Some(path) = file_path(target, key) else { return status(out, "404 Not Found", "") };
    if !allowed(&path) {
        return status(out, "403 Forbidden", "");
    }
    let Ok(mut file) = File::open(&path) else { return status(out, "404 Not Found", "") };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return status(out, "404 Not Found", "");
    }
    let size = metadata.len();
    let range = lines[1..].iter().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("range").then(|| value.trim().to_string())
    });
    // One range is what players ask for; several, or one that can't be read, get the whole file.
    let (start, length, partial) = match range.map(|range| HttpRange::parse(&range, size)) {
        Some(Ok(ranges)) if ranges.len() == 1 => (ranges[0].start, ranges[0].length, true),
        Some(Err(HttpRangeParseError::NoOverlap)) => {
            return status(out, "416 Range Not Satisfiable", &format!("Content-Range: bytes */{size}\r\n"));
        }
        _ => (0, size, false),
    };
    let mut reply = if partial {
        format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{size}\r\n", start + length - 1)
    } else {
        "HTTP/1.1 200 OK\r\n".to_string()
    };
    reply.push_str(&format!(
        "Content-Type: {}\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        content_type(&path)
    ));
    out.write_all(reply.as_bytes())?;
    if !only_head {
        file.seek(SeekFrom::Start(start))?;
        io::copy(&mut file.take(length), out)?;
    }
    out.flush()
}

/// The file a request's target names (`/<key>/<path, percent-encoded>`), when the key is this server's and the path
/// is a whole one.
fn file_path(target: &str, key: &str) -> Option<PathBuf> {
    let rest = target.strip_prefix('/')?.split('?').next()?;
    let (given, encoded) = rest.split_once('/')?;
    if !same(given.as_bytes(), key.as_bytes()) {
        return None;
    }
    let path = PathBuf::from(OsString::from_vec(percent_decode_str(encoded).collect()));
    (path.is_absolute() && !path.components().any(|part| part == Component::ParentDir)).then_some(path)
}

/// Compares the key in the same time however much of it matches.
fn same(given: &[u8], key: &[u8]) -> bool {
    given.len() == key.len() && given.iter().zip(key).fold(0, |differ, (a, b)| differ | (a ^ b)) == 0
}

/// A reply with no body.
fn status(out: &mut impl Write, status: &str, headers: &str) -> io::Result<()> {
    write!(out, "HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n")?;
    out.flush()
}

/// What a file is, by its extension (GStreamer looks inside anyway; WebKit goes by this first).
fn content_type(path: &Path) -> &'static str {
    let extension = path.extension().and_then(|extension| extension.to_str()).unwrap_or("").to_ascii_lowercase();
    match extension.as_str() {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    /// A folder with a 100-byte file whose name needs encoding.
    fn folder(name: &str) -> (PathBuf, PathBuf) {
        let folder = std::env::temp_dir().join(format!("digup-media-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("a talk ł.wav");
        std::fs::write(&file, (0..100u8).collect::<Vec<u8>>()).unwrap();
        (folder, file)
    }

    fn encoded(path: &Path) -> String {
        percent_encoding::utf8_percent_encode(path.to_str().unwrap(), percent_encoding::NON_ALPHANUMERIC).to_string()
    }

    /// The reply to `request` (its head), as the status line, the headers and the body.
    fn ask(request: &str, allowed: &dyn Fn(&Path) -> bool) -> (String, String, Vec<u8>) {
        let mut out = Vec::new();
        answer(&mut request.as_bytes(), KEY, allowed, &mut out).unwrap();
        let split = out.windows(4).position(|four| four == b"\r\n\r\n").expect("a whole head");
        let head = String::from_utf8(out[..split].to_vec()).unwrap();
        let (first, headers) = head.split_once("\r\n").unwrap_or((&head, ""));
        (first.to_string(), headers.to_string(), out[split + 4..].to_vec())
    }

    fn get(target: &str, extra: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra}\r\n")
    }

    #[test]
    fn serves_the_whole_file() {
        let (folder, file) = folder("whole");
        let inside = |path: &Path| path.starts_with(&folder);
        let (first, headers, body) = ask(&get(&format!("/{KEY}/{}", encoded(&file)), ""), &inside);
        assert_eq!(first, "HTTP/1.1 200 OK");
        assert!(headers.contains("Content-Type: audio/wav"), "{headers}");
        assert!(headers.contains("Content-Length: 100"), "{headers}");
        assert!(headers.contains("Accept-Ranges: bytes"), "{headers}");
        assert_eq!(body, (0..100u8).collect::<Vec<u8>>());
    }

    #[test]
    fn serves_the_range_asked_for() {
        let (folder, file) = folder("range");
        let inside = |path: &Path| path.starts_with(&folder);
        let target = format!("/{KEY}/{}", encoded(&file));
        let (first, headers, body) = ask(&get(&target, "Range: bytes=10-19\r\n"), &inside);
        assert_eq!(first, "HTTP/1.1 206 Partial Content");
        assert!(headers.contains("Content-Range: bytes 10-19/100"), "{headers}");
        assert!(headers.contains("Content-Length: 10"), "{headers}");
        assert_eq!(body, (10..20u8).collect::<Vec<u8>>());

        let (first, headers, body) = ask(&get(&target, "range: bytes=90-\r\n"), &inside);
        assert_eq!(first, "HTTP/1.1 206 Partial Content");
        assert!(headers.contains("Content-Range: bytes 90-99/100"), "{headers}");
        assert_eq!(body, (90..100u8).collect::<Vec<u8>>());

        let (first, headers, body) = ask(&get(&target, "Range: bytes=-5\r\n"), &inside);
        assert_eq!(first, "HTTP/1.1 206 Partial Content");
        assert!(headers.contains("Content-Range: bytes 95-99/100"), "{headers}");
        assert_eq!(body, (95..100u8).collect::<Vec<u8>>());

        let (first, headers, body) = ask(&get(&target, "Range: bytes=200-\r\n"), &inside);
        assert_eq!(first, "HTTP/1.1 416 Range Not Satisfiable");
        assert!(headers.contains("Content-Range: bytes */100"), "{headers}");
        assert!(body.is_empty());
    }

    #[test]
    fn head_has_no_body() {
        let (folder, file) = folder("head");
        let inside = |path: &Path| path.starts_with(&folder);
        let (first, headers, body) = ask(&format!("HEAD /{KEY}/{} HTTP/1.1\r\n\r\n", encoded(&file)), &inside);
        assert_eq!(first, "HTTP/1.1 200 OK");
        assert!(headers.contains("Content-Length: 100"), "{headers}");
        assert!(body.is_empty());
    }

    #[test]
    fn refuses_a_wrong_key_a_file_outside_the_scope_and_other_methods() {
        let (folder, file) = folder("refuse");
        let inside = |path: &Path| path.starts_with(&folder);
        let wrong = "f".repeat(KEY.len());
        assert_eq!(ask(&get(&format!("/{wrong}/{}", encoded(&file)), ""), &inside).0, "HTTP/1.1 404 Not Found");
        assert_eq!(ask(&get(&format!("/{}", encoded(&file)), ""), &inside).0, "HTTP/1.1 404 Not Found");
        let nowhere = |_: &Path| false;
        assert_eq!(ask(&get(&format!("/{KEY}/{}", encoded(&file)), ""), &nowhere).0, "HTTP/1.1 403 Forbidden");
        // A way out of an allowed folder isn't a path at all.
        let out = folder.join("..").join("elsewhere.wav");
        assert_eq!(ask(&get(&format!("/{KEY}/{}", encoded(&out)), ""), &inside).0, "HTTP/1.1 404 Not Found");
        assert_eq!(ask(&get(&format!("/{KEY}/relative.wav"), ""), &|_: &Path| true).0, "HTTP/1.1 404 Not Found");
        assert_eq!(ask(&get(&format!("/{KEY}/{}", encoded(&folder)), ""), &|_: &Path| true).0, "HTTP/1.1 404 Not Found");
        let post = format!("POST /{KEY}/{} HTTP/1.1\r\n\r\n", encoded(&file));
        assert_eq!(ask(&post, &inside).0, "HTTP/1.1 405 Method Not Allowed");
    }

    #[test]
    fn answers_over_the_loopback() {
        let (folder, file) = folder("loopback");
        let address = serve_where(move |path| path.starts_with(&folder)).unwrap();
        let rest = address.strip_prefix("http://").unwrap();
        let (host, base) = rest.split_once('/').unwrap();
        assert!(host.starts_with("127.0.0.1:"), "{address}");
        let key = base.trim_end_matches('/');
        assert_eq!(key.len(), 32);
        let mut stream = TcpStream::connect(host).unwrap();
        write!(stream, "GET /{key}/{} HTTP/1.1\r\nRange: bytes=0-3\r\n\r\n", encoded(&file)).unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        assert!(reply.starts_with(b"HTTP/1.1 206 Partial Content\r\n"), "{}", String::from_utf8_lossy(&reply));
        assert!(reply.ends_with(&[0, 1, 2, 3]));
        assert_ne!(new_key().unwrap(), key);
    }
}
