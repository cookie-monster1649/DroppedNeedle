//! Fork: fetch one song's audio from a YouTube or SoundCloud page with
//! yt-dlp, for a manual import onto a still-hunting song.
//!
//! Only pages on youtube.com, youtu.be, music.youtube.com and
//! soundcloud.com are fetched. yt-dlp runs as a program with its
//! arguments, never through a shell, with no config file, one item only,
//! a size cap and a time limit. The best audio is kept in the codec it
//! came in: Opus in a WebM is moved into an Ogg file by ffmpeg with the
//! audio copied, never re-encoded, and AAC (M4A) and MP3 stay as they are.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use reqwest::Url;
use tokio::io::AsyncBufReadExt as _;
use tokio::process::Command;

/// Largest audio file fetched.
pub const MAX_FETCH_BYTES: u64 = 200 * 1024 * 1024;
/// Longest a fetch may run, remux included.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Hosts a link may name.
const HOSTS: [&str; 7] = [
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "music.youtube.com",
    "youtu.be",
    "soundcloud.com",
    "m.soundcloud.com",
];

/// Where a link points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSite {
    YouTube,
    SoundCloud,
}

impl LinkSite {
    pub fn source(self) -> &'static str {
        match self {
            Self::YouTube => "youtube",
            Self::SoundCloud => "soundcloud",
        }
    }
}

/// A link checked for fetching: http(s), one of the allowed hosts, no
/// credentials or odd port. Answers it as yt-dlp is given it.
pub fn allowed_link(raw: &str) -> Result<(Url, LinkSite), String> {
    let refused =
        || "Paste a youtube.com, youtu.be, music.youtube.com or soundcloud.com link.".to_owned();
    let url = Url::parse(raw.trim()).map_err(|_| refused())?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(refused());
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if !HOSTS.contains(&host.as_str()) {
        return Err(refused());
    }
    let site = if host.ends_with("soundcloud.com") {
        LinkSite::SoundCloud
    } else {
        LinkSite::YouTube
    };
    Ok((url, site))
}

/// The yt-dlp program: `DROPPEDNEEDLE_YTDLP`, else `yt-dlp` on the path.
fn ytdlp() -> PathBuf {
    std::env::var_os("DROPPEDNEEDLE_YTDLP").map_or_else(|| PathBuf::from("yt-dlp"), PathBuf::from)
}

/// The ffmpeg program: `DROPPEDNEEDLE_FFMPEG`, else `ffmpeg` on the path.
fn ffmpeg() -> PathBuf {
    std::env::var_os("DROPPEDNEEDLE_FFMPEG").map_or_else(|| PathBuf::from("ffmpeg"), PathBuf::from)
}

/// The arguments yt-dlp runs with for `url`, writing into `dir`.
pub fn ytdlp_args(url: &Url, dir: &Path) -> Vec<String> {
    vec![
        "--ignore-config".into(),
        "--no-playlist".into(),
        "--no-cache-dir".into(),
        "--no-mtime".into(),
        "--no-part".into(),
        "--newline".into(),
        "--no-colors".into(),
        "--socket-timeout".into(),
        "30".into(),
        "--max-filesize".into(),
        MAX_FETCH_BYTES.to_string(),
        "--format".into(),
        "bestaudio/best[vcodec=none]".into(),
        "--progress-template".into(),
        "download:DNPROGRESS %(progress._percent_str)s".into(),
        "--output".into(),
        format!("{}/audio.%(ext)s", dir.display()),
        "--".into(),
        url.as_str().to_owned(),
    ]
}

/// Fetch `url` into `dir` (which must exist and be empty), reporting the
/// percent downloaded. Answers the audio file, ready to import.
pub async fn fetch_audio(
    url: &Url,
    dir: &Path,
    progress: impl Fn(f64) + Send,
) -> Result<PathBuf, String> {
    match tokio::time::timeout(FETCH_TIMEOUT, run(url, dir, progress)).await {
        Ok(result) => result,
        Err(_) => Err("The download took too long and was stopped.".to_owned()),
    }
}

async fn run(url: &Url, dir: &Path, progress: impl Fn(f64) + Send) -> Result<PathBuf, String> {
    let mut child = Command::new(ytdlp())
        .args(ytdlp_args(url, dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            tracing::warn!(%error, "yt-dlp could not start");
            "The downloader is not available on this server.".to_owned()
        })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let errors = tokio::spawn(async move {
        let mut last = String::new();
        if let Some(stderr) = stderr {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.starts_with("ERROR") {
                    last = line;
                }
            }
        }
        last
    });
    if let Some(stdout) = stdout {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(percent) = parse_progress(&line) {
                progress(percent);
            }
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("The downloader stopped: {error}"))?;
    let error = errors.await.unwrap_or_default();
    if !status.success() {
        tracing::warn!(%error, code = status.code(), "yt-dlp failed");
        return Err(plain_error(&error));
    }
    let fetched = only_file(dir)?;
    let size = std::fs::metadata(&fetched).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Err("Nothing was downloaded from that link.".to_owned());
    }
    if size > MAX_FETCH_BYTES {
        return Err("That audio is larger than 200 MB.".to_owned());
    }
    finish(&fetched).await
}

/// One progress line's percent.
pub fn parse_progress(line: &str) -> Option<f64> {
    let rest = line.trim().strip_prefix("DNPROGRESS")?.trim();
    rest.trim_end_matches('%').trim().parse::<f64>().ok()
}

/// yt-dlp's error as a sentence a person can act on.
fn plain_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("max-filesize") || lower.contains("larger than max") {
        "That audio is larger than 200 MB.".to_owned()
    } else if lower.contains("private") || lower.contains("unavailable") {
        "That video or track is private or unavailable.".to_owned()
    } else if lower.contains("sign in") || lower.contains("age") {
        "That page needs a signed-in account, which this server does not have.".to_owned()
    } else if error.is_empty() {
        "The download failed.".to_owned()
    } else {
        let detail = error.trim_start_matches("ERROR:").trim();
        format!(
            "The download failed: {}",
            detail.chars().take(200).collect::<String>()
        )
    }
}

/// The one file yt-dlp left in `dir`.
fn only_file(dir: &Path) -> Result<PathBuf, String> {
    let mut files = std::fs::read_dir(dir)
        .map_err(|_| "Nothing was downloaded from that link.".to_owned())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file());
    files
        .next()
        .ok_or_else(|| "Nothing was downloaded from that link.".to_owned())
}

/// The file as the library takes it: Opus out of a WebM into Ogg with the
/// audio copied; M4A, MP3, Ogg and Opus as they are.
async fn finish(fetched: &Path) -> Result<PathBuf, String> {
    let ext = fetched
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "m4a" | "mp3" | "opus" | "ogg" => Ok(fetched.to_path_buf()),
        "webm" | "mka" => {
            let out = fetched.with_extension("opus");
            let status = Command::new(ffmpeg())
                .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
                .arg(fetched)
                .args(["-vn", "-map", "0:a:0", "-c:a", "copy", "-f", "ogg"])
                .arg(&out)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status()
                .await
                .map_err(|_| "The audio could not be unpacked on this server.".to_owned())?;
            let _ = std::fs::remove_file(fetched);
            if !status.success() {
                let _ = std::fs::remove_file(&out);
                return Err("The audio is not Opus, AAC or MP3, so it was not kept.".to_owned());
            }
            Ok(out)
        }
        _ => Err(format!(
            "That link gave a .{ext} file, which is not Opus, AAC or MP3, so it was not kept."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_four_sites_are_fetched() {
        for good in [
            "https://www.youtube.com/watch?v=abc",
            "https://youtu.be/abc",
            "https://music.youtube.com/watch?v=abc",
            "https://soundcloud.com/artist/song",
            "http://m.youtube.com/watch?v=abc",
        ] {
            assert!(allowed_link(good).is_ok(), "{good}");
        }
        for bad in [
            "https://evil.com/watch?v=abc",
            "https://youtube.com.evil.com/x",
            "https://user:pw@youtube.com/watch?v=abc",
            "https://youtube.com:8443/watch?v=abc",
            "file:///etc/passwd",
            "ftp://youtube.com/x",
            "--exec rm",
            "",
        ] {
            assert!(allowed_link(bad).is_err(), "{bad}");
        }
        assert_eq!(
            allowed_link("https://soundcloud.com/a/b").map(|(_, site)| site),
            Ok(LinkSite::SoundCloud)
        );
    }

    #[test]
    fn the_link_goes_after_the_end_of_options() {
        let (url, _) = allowed_link("https://youtu.be/abc").expect("link");
        let args = ytdlp_args(&url, Path::new("/tmp/x"));
        let end = args
            .iter()
            .position(|arg| arg == "--")
            .expect("end of options");
        assert_eq!(args[end + 1], "https://youtu.be/abc");
        assert_eq!(args.len(), end + 2);
        assert!(args.contains(&"--ignore-config".to_owned()));
        assert!(args.contains(&"--no-playlist".to_owned()));
    }

    #[test]
    fn progress_lines_read_as_percent() {
        assert_eq!(parse_progress("DNPROGRESS  42.5%"), Some(42.5));
        assert_eq!(parse_progress("[download] 10%"), None);
    }

    #[tokio::test]
    async fn native_codecs_are_kept_and_others_refused() {
        let dir = std::env::temp_dir().join(format!("dn-fetch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let m4a = dir.join("audio.m4a");
        std::fs::write(&m4a, b"x").expect("file");
        assert_eq!(finish(&m4a).await, Ok(m4a.clone()));
        let wav = dir.join("audio.wav");
        std::fs::write(&wav, b"x").expect("file");
        assert!(finish(&wav).await.is_err(), "no fake lossless");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
