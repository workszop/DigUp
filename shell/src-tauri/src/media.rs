//! Sound and video for the panel's previews, on Linux. WebKitGTK plays media through GStreamer and hands it the media's
//! URL as it is: it lets through only http(s), file, blob and data URLs, and GStreamer has no source for Tauri's
//! `asset://` either, so an `<audio>` or `<video>` given a `convertFileSrc` URL fails at once (MEDIA_ERR_SRC_NOT_SUPPORTED)
//! while pictures from the same folders show. The previews' media comes from here instead: a server on the loopback
//! whose address carries a random key only the panel's page is told, serving only what the asset protocol's scope
//! allows, in the byte ranges a player asks for.

use http_range::{HttpRange, HttpRangeParseError};
use percent_encoding::percent_decode_str;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// A request's head is a line and a few headers; anything longer isn't from the panel.
const MAX_HEAD: u64 = 16 * 1024;
/// How long the listener waits after a failed accept (out of file descriptors, say) before it tries again.
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);

/// What the server lets its clients take: no client, keyed or not, can tie up the app's threads or files.
#[derive(Clone, Copy)]
struct Limits {
    /// Connections open at once; more are closed as they come. A preview needs one or two.
    connections: usize,
    /// How long a connection has to send its whole request head.
    head_time: Duration,
    /// How long one write may wait on a client that doesn't read.
    write_time: Duration,
}

const LIMITS: Limits = Limits {
    connections: 32,
    head_time: Duration::from_secs(10),
    write_time: Duration::from_secs(30),
};

/// What a request is checked against.
struct Server {
    key: String,
    /// The Host a request must name (`127.0.0.1:<port>`): a page that rebinds its own name to the loopback names its own.
    host: String,
    allowed: Box<dyn Fn(&Path) -> bool + Send + Sync>,
}

/// Starts the server for the files `scope` allows; the URL of a file is the address this returns followed by its
/// path, percent-encoded as one component (`encodeURIComponent`).
pub fn serve(scope: tauri::scope::fs::Scope) -> io::Result<String> {
    serve_with(LIMITS, move |path| scope.is_allowed(path))
}

/// `serve`, within `limits`, with `allowed` saying which files (by their real paths) may be read.
fn serve_with(limits: Limits, allowed: impl Fn(&Path) -> bool + Send + Sync + 'static) -> io::Result<String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let server = Arc::new(Server { key: new_key()?, host: format!("127.0.0.1:{port}"), allowed: Box::new(allowed) });
    let address = format!("http://{}/{}/", server.host, server.key);
    let open = Arc::new(AtomicUsize::new(0));
    thread::Builder::new().name("media".into()).spawn(move || loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(_) => {
                thread::sleep(ACCEPT_PAUSE);
                continue;
            }
        };
        // Past the limit the connection is dropped, which closes it.
        let Some(slot) = Slot::take(&open, limits.connections) else { continue };
        let server = server.clone();
        // A player reads a stream for as long as it plays, and asks again for each seek: one thread a connection.
        let _ = thread::Builder::new().name("media-reply".into()).spawn(move || {
            let _slot = slot;
            let _ = stream.set_write_timeout(Some(limits.write_time));
            let head = Deadline { stream: &stream, until: Instant::now() + limits.head_time };
            let _ = answer(&mut BufReader::new(head), &server, &mut &stream);
        });
    })?;
    Ok(address)
}

/// 128 random bits as hex: no other program or page can guess the server's addresses.
fn new_key() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// One of the connections the server allows at once, given back when its connection ends.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(open: &Arc<AtomicUsize>, most: usize) -> Option<Slot> {
        if open.fetch_add(1, Ordering::SeqCst) >= most {
            open.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(open.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A connection's request, read until a deadline for all of it: a client sending a byte now and then can't keep it
/// open, as it could with a timeout for each read.
struct Deadline<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl Read for Deadline<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        let mut stream = self.stream;
        stream.read(buffer)
    }
}

// MARK: One request

/// Reads a request from `request` and writes its answer to `out`: the file (or the range of it asked for) when the key
/// and the Host are the server's and the file's real path is one it allows. A request without them learns nothing,
/// not even which methods there are: it gets the same 404 whatever it asks.
fn answer(request: &mut impl BufRead, server: &Server, out: &mut impl Write) -> io::Result<()> {
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
    let header = |wanted: &str| {
        lines[1..].iter().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim().eq_ignore_ascii_case(wanted).then(|| value.trim().to_string())
        })
    };
    let mut words = first.split(' ');
    let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let Some(path) = file_path(target, &server.key) else { return status(out, "404 Not Found", "") };
    if header("host").as_deref() != Some(server.host.as_str()) {
        return status(out, "404 Not Found", "");
    }
    let only_head = method == "HEAD";
    if method != "GET" && !only_head {
        return status(out, "405 Method Not Allowed", "");
    }
    // The scope is asked about the file's real path, and that path is what's opened, never through a link: a link
    // can't lead out of the scope, nor be swapped in between the check and the open.
    let Ok(real) = path.canonicalize() else { return status(out, "404 Not Found", "") };
    if !(server.allowed)(&real) {
        return status(out, "403 Forbidden", "");
    }
    let opened = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&real);
    let Ok(mut file) = opened else { return status(out, "404 Not Found", "") };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return status(out, "404 Not Found", "");
    }
    let size = metadata.len();
    let unsatisfiable = format!("Content-Range: bytes */{size}\r\n");
    // One range is what players ask for; several, or one that can't be read, get the whole file. No range of an empty
    // file has a byte in it.
    let (start, length, partial) = match header("range").map(|range| HttpRange::parse(&range, size)) {
        None => (0, size, false),
        Some(_) if size == 0 => return status(out, "416 Range Not Satisfiable", &unsatisfiable),
        Some(Ok(ranges)) if ranges.len() == 1 && ranges[0].length > 0 => (ranges[0].start, ranges[0].length, true),
        Some(Ok(ranges)) if ranges.len() == 1 => return status(out, "416 Range Not Satisfiable", &unsatisfiable),
        Some(Err(HttpRangeParseError::NoOverlap)) => return status(out, "416 Range Not Satisfiable", &unsatisfiable),
        Some(_) => (0, size, false),
    };
    let mut reply = if partial {
        format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{size}\r\n", start + length - 1)
    } else {
        "HTTP/1.1 200 OK\r\n".to_string()
    };
    reply.push_str(&format!(
        "Content-Type: {}\r\nContent-Length: {length}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        content_type(&real)
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

    const KEY: &str = "0123456789abcdef0123456789abcdef";
    const HOST: &str = "127.0.0.1:1";

    /// A folder of the test's own, removed when the test ends.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Folder {
            let folder = std::env::temp_dir().join(format!("digup-media-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&folder);
            std::fs::create_dir_all(&folder).unwrap();
            Folder(folder.canonicalize().unwrap())
        }

        /// A 100-byte file whose name needs encoding.
        fn file(&self) -> PathBuf {
            let file = self.0.join("a talk ł.wav");
            std::fs::write(&file, (0..100u8).collect::<Vec<u8>>()).unwrap();
            file
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A server allowing what's in `folder`.
    fn inside(folder: &Folder) -> Server {
        let folder = folder.0.clone();
        Server { key: KEY.into(), host: HOST.into(), allowed: Box::new(move |path: &Path| path.starts_with(&folder)) }
    }

    fn encoded(path: &Path) -> String {
        percent_encoding::utf8_percent_encode(path.to_str().unwrap(), percent_encoding::NON_ALPHANUMERIC).to_string()
    }

    /// The reply to `request` (its head), as the status line, the headers and the body.
    fn ask(request: &str, server: &Server) -> (String, String, Vec<u8>) {
        let mut out = Vec::new();
        answer(&mut request.as_bytes(), server, &mut out).unwrap();
        let split = out.windows(4).position(|four| four == b"\r\n\r\n").expect("a whole head");
        let head = String::from_utf8(out[..split].to_vec()).unwrap();
        let (first, headers) = head.split_once("\r\n").unwrap_or((&head, ""));
        (first.to_string(), headers.to_string(), out[split + 4..].to_vec())
    }

    fn get(target: &str, extra: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: {HOST}\r\n{extra}\r\n")
    }

    #[test]
    fn serves_the_whole_file() {
        let folder = Folder::new("whole");
        let file = folder.file();
        let (first, headers, body) = ask(&get(&format!("/{KEY}/{}", encoded(&file)), ""), &inside(&folder));
        assert_eq!(first, "HTTP/1.1 200 OK");
        assert!(headers.contains("Content-Type: audio/wav"), "{headers}");
        assert!(headers.contains("Content-Length: 100"), "{headers}");
        assert!(headers.contains("Accept-Ranges: bytes"), "{headers}");
        assert_eq!(body, (0..100u8).collect::<Vec<u8>>());
    }

    #[test]
    fn serves_the_range_asked_for() {
        let folder = Folder::new("range");
        let server = inside(&folder);
        let target = format!("/{KEY}/{}", encoded(&folder.file()));
        let (first, headers, body) = ask(&get(&target, "Range: bytes=10-19\r\n"), &server);
        assert_eq!(first, "HTTP/1.1 206 Partial Content");
        assert!(headers.contains("Content-Range: bytes 10-19/100"), "{headers}");
        assert!(headers.contains("Content-Length: 10"), "{headers}");
        assert_eq!(body, (10..20u8).collect::<Vec<u8>>());

        let (first, headers, body) = ask(&get(&target, "range: bytes=90-\r\n"), &server);
        assert_eq!(first, "HTTP/1.1 206 Partial Content");
        assert!(headers.contains("Content-Range: bytes 90-99/100"), "{headers}");
        assert_eq!(body, (90..100u8).collect::<Vec<u8>>());

        let (first, headers, body) = ask(&get(&target, "Range: bytes=-5\r\n"), &server);
        assert_eq!(first, "HTTP/1.1 206 Partial Content");
        assert!(headers.contains("Content-Range: bytes 95-99/100"), "{headers}");
        assert_eq!(body, (95..100u8).collect::<Vec<u8>>());

        let (first, headers, body) = ask(&get(&target, "Range: bytes=200-\r\n"), &server);
        assert_eq!(first, "HTTP/1.1 416 Range Not Satisfiable");
        assert!(headers.contains("Content-Range: bytes */100"), "{headers}");
        assert!(body.is_empty());
    }

    #[test]
    fn odd_ranges_get_the_whole_file() {
        let folder = Folder::new("odd");
        let server = inside(&folder);
        let target = format!("/{KEY}/{}", encoded(&folder.file()));
        for range in ["bytes=0-1,5-6", "bytes=20-10", "bytes=x-y", "pages=1-2", "bytes=", "bytes=--5"] {
            let (first, headers, body) = ask(&get(&target, &format!("Range: {range}\r\n")), &server);
            assert_eq!(first, "HTTP/1.1 200 OK", "{range}");
            assert!(headers.contains("Content-Length: 100"), "{range}: {headers}");
            assert_eq!(body.len(), 100, "{range}");
        }
    }

    #[test]
    fn no_range_of_an_empty_file_is_satisfiable() {
        let folder = Folder::new("empty");
        let empty = folder.0.join("empty.wav");
        std::fs::write(&empty, b"").unwrap();
        let server = inside(&folder);
        let target = format!("/{KEY}/{}", encoded(&empty));
        for range in ["bytes=-5", "bytes=0-", "bytes=0-0", "bytes=x"] {
            let (first, headers, body) = ask(&get(&target, &format!("Range: {range}\r\n")), &server);
            assert_eq!(first, "HTTP/1.1 416 Range Not Satisfiable", "{range}");
            assert!(headers.contains("Content-Range: bytes */0"), "{range}: {headers}");
            assert!(body.is_empty());
        }
        let (first, headers, body) = ask(&get(&target, ""), &server);
        assert_eq!(first, "HTTP/1.1 200 OK");
        assert!(headers.contains("Content-Length: 0"), "{headers}");
        assert!(body.is_empty());
    }

    #[test]
    fn head_has_no_body() {
        let folder = Folder::new("head");
        let request = format!("HEAD /{KEY}/{} HTTP/1.1\r\nHost: {HOST}\r\n\r\n", encoded(&folder.file()));
        let (first, headers, body) = ask(&request, &inside(&folder));
        assert_eq!(first, "HTTP/1.1 200 OK");
        assert!(headers.contains("Content-Length: 100"), "{headers}");
        assert!(body.is_empty());
    }

    #[test]
    fn without_the_key_and_host_every_request_gets_the_same_404() {
        let folder = Folder::new("unkeyed");
        let server = inside(&folder);
        let file = encoded(&folder.file());
        let wrong = "f".repeat(KEY.len());
        for request in [
            get(&format!("/{wrong}/{file}"), ""),
            get(&format!("/{file}"), ""),
            format!("POST /{wrong}/{file} HTTP/1.1\r\nHost: {HOST}\r\n\r\n"),
            format!("DELETE / HTTP/1.1\r\nHost: {HOST}\r\n\r\n"),
            format!("GET /{KEY}/{file} HTTP/1.1\r\nHost: digup.example:80\r\n\r\n"),
            format!("GET /{KEY}/{file} HTTP/1.1\r\nHost: 127.0.0.1:2\r\n\r\n"),
            format!("GET /{KEY}/{file} HTTP/1.1\r\n\r\n"),
            format!("POST /{KEY}/{file} HTTP/1.1\r\nHost: evil.example\r\n\r\n"),
        ] {
            assert_eq!(ask(&request, &server), ("HTTP/1.1 404 Not Found".into(), "Content-Length: 0\r\nConnection: close".into(), vec![]), "{request}");
        }
        // With both, a method other than GET or HEAD is turned down as such.
        let post = format!("POST /{KEY}/{file} HTTP/1.1\r\nHost: {HOST}\r\n\r\n");
        assert_eq!(ask(&post, &server).0, "HTTP/1.1 405 Method Not Allowed");
    }

    #[test]
    fn refuses_files_outside_the_scope_and_odd_paths() {
        let folder = Folder::new("refuse");
        let server = inside(&folder);
        let file = folder.file();
        let nowhere = Server { key: KEY.into(), host: HOST.into(), allowed: Box::new(|_: &Path| false) };
        assert_eq!(ask(&get(&format!("/{KEY}/{}", encoded(&file)), ""), &nowhere).0, "HTTP/1.1 403 Forbidden");
        let anything = Server { key: KEY.into(), host: HOST.into(), allowed: Box::new(|_: &Path| true) };
        // A way out of an allowed folder isn't a path at all; nor is a relative one.
        let out = folder.0.join("..").join("elsewhere.wav");
        assert_eq!(ask(&get(&format!("/{KEY}/{}", encoded(&out)), ""), &server).0, "HTTP/1.1 404 Not Found");
        assert_eq!(ask(&get(&format!("/{KEY}/relative.wav"), ""), &anything).0, "HTTP/1.1 404 Not Found");
        // A folder isn't a file.
        assert_eq!(ask(&get(&format!("/{KEY}/{}", encoded(&folder.0)), ""), &anything).0, "HTTP/1.1 404 Not Found");
        // A NUL ends no path here: there's no file by that name.
        let nul = format!("/{KEY}/{}%00.txt", encoded(&file));
        assert_eq!(ask(&get(&nul, ""), &anything).0, "HTTP/1.1 404 Not Found");
        // Encoded twice, the path is decoded once: "%2F…" is no absolute path.
        let twice = percent_encoding::utf8_percent_encode(&encoded(&file), percent_encoding::NON_ALPHANUMERIC).to_string();
        assert_eq!(ask(&get(&format!("/{KEY}/{twice}"), ""), &anything).0, "HTTP/1.1 404 Not Found");
    }

    #[test]
    fn a_link_is_followed_only_to_where_the_scope_allows() {
        let folder = Folder::new("links");
        let elsewhere = Folder::new("links-elsewhere");
        let secret = elsewhere.0.join("secret.wav");
        std::fs::write(&secret, b"secret").unwrap();
        let out = folder.0.join("out.wav");
        std::os::unix::fs::symlink(&secret, &out).unwrap();
        let server = inside(&folder);
        assert_eq!(ask(&get(&format!("/{KEY}/{}", encoded(&out)), ""), &server).0, "HTTP/1.1 403 Forbidden");
        // A link whose own path is relative (taken from its folder, not the program's).
        let relative = folder.0.join("relative.wav");
        let up = Path::new("..").join(elsewhere.0.file_name().unwrap()).join("secret.wav");
        std::os::unix::fs::symlink(&up, &relative).unwrap();
        assert_eq!(ask(&get(&format!("/{KEY}/{}", encoded(&relative)), ""), &server).0, "HTTP/1.1 403 Forbidden");
        // A link that stays inside is fine.
        let file = folder.file();
        let within = folder.0.join("within.wav");
        std::os::unix::fs::symlink(&file, &within).unwrap();
        let (first, _, body) = ask(&get(&format!("/{KEY}/{}", encoded(&within)), ""), &server);
        assert_eq!(first, "HTTP/1.1 200 OK");
        assert_eq!(body.len(), 100);
    }

    // MARK: Over the loopback

    /// The server's host and key, from its address.
    fn parts(address: &str) -> (String, String) {
        let rest = address.strip_prefix("http://").unwrap();
        let (host, key) = rest.split_once('/').unwrap();
        (host.to_string(), key.trim_end_matches('/').to_string())
    }

    /// The whole reply to `request` on a new connection.
    fn fetch(host: &str, request: &str) -> io::Result<Vec<u8>> {
        let mut stream = TcpStream::connect(host)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.write_all(request.as_bytes())?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply)?;
        Ok(reply)
    }

    /// Whether the server closes `stream` (rather than leaving it waiting) within `within`.
    fn closed_within(stream: &mut TcpStream, within: Duration) -> bool {
        stream.set_read_timeout(Some(within)).unwrap();
        match stream.read(&mut [0u8; 64]) {
            Ok(0) => true,
            Ok(_) => false,
            Err(error) => !matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut),
        }
    }

    #[test]
    fn answers_over_the_loopback() {
        let folder = Folder::new("loopback");
        let file = folder.file();
        let allowed = folder.0.clone();
        let address = serve_with(LIMITS, move |path| path.starts_with(&allowed)).unwrap();
        let (host, key) = parts(&address);
        assert!(host.starts_with("127.0.0.1:"), "{address}");
        assert_eq!(key.len(), 32);
        let request = format!("GET /{key}/{} HTTP/1.1\r\nHost: {host}\r\nRange: bytes=0-3\r\n\r\n", encoded(&file));
        let reply = fetch(&host, &request).unwrap();
        assert!(reply.starts_with(b"HTTP/1.1 206 Partial Content\r\n"), "{}", String::from_utf8_lossy(&reply));
        assert!(reply.ends_with(&[0, 1, 2, 3]));
        assert_ne!(new_key().unwrap(), key);
    }

    #[test]
    fn connections_past_the_limit_are_closed_at_once() {
        let folder = Folder::new("limit");
        let file = folder.file();
        let allowed = folder.0.clone();
        let limits = Limits { connections: 2, head_time: Duration::from_secs(5), write_time: LIMITS.write_time };
        let address = serve_with(limits, move |path| path.starts_with(&allowed)).unwrap();
        let (host, key) = parts(&address);
        // Two clients that say nothing hold both connections…
        let first = TcpStream::connect(&host).unwrap();
        let second = TcpStream::connect(&host).unwrap();
        // …so a third is closed, not kept waiting.
        let mut third = TcpStream::connect(&host).unwrap();
        assert!(closed_within(&mut third, Duration::from_secs(2)));
        // Once they go, there's room again.
        drop((first, second));
        let request = format!("GET /{key}/{} HTTP/1.1\r\nHost: {host}\r\n\r\n", encoded(&file));
        let started = Instant::now();
        loop {
            let reply = fetch(&host, &request).unwrap_or_default();
            if reply.starts_with(b"HTTP/1.1 200 OK\r\n") {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(3), "no room after the clients left");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_slow_client_is_cut_off_at_the_deadline() {
        let limits = Limits { connections: 4, head_time: Duration::from_millis(300), write_time: LIMITS.write_time };
        let address = serve_with(limits, |_| true).unwrap();
        let (host, _) = parts(&address);
        let mut slow = TcpStream::connect(&host).unwrap();
        let started = Instant::now();
        // A byte every 100 ms: each read would be quick, the whole head never comes.
        let cut = loop {
            if slow.write_all(b"G").is_err() || closed_within(&mut slow, Duration::from_millis(100)) {
                break started.elapsed();
            }
            assert!(started.elapsed() < Duration::from_secs(3), "the slow client was never cut off");
        };
        assert!(cut >= Duration::from_millis(250) && cut < Duration::from_millis(1500), "{cut:?}");
    }
}
