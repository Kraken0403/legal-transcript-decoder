//! Bounded, local coordinate/OCR adapter. No hosted service receives document bytes.
use super::models::ExtractedPage;
use serde::Deserialize;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
struct AdapterResult {
    protocol_version: u32,
    pages: Vec<ExtractedPage>,
}

struct TemporaryOutput(std::path::PathBuf);
impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn extract(bytes: &[u8]) -> Result<Vec<ExtractedPage>, String> {
    let configured = std::env::var("TRANSCRIPT_PYTHON").ok();
    let candidates: Vec<&str> = match configured.as_deref() {
        Some(path) => vec![path],
        None if cfg!(windows) => vec!["python", "py"],
        None => vec!["python3", "python"],
    };
    let mut errors = Vec::new();
    for executable in candidates {
        match run(executable, bytes) {
            Ok(pages) => return Ok(pages),
            Err(error) => errors.push(error),
        }
    }
    Err(errors.join(" | "))
}

fn run(executable: &str, bytes: &[u8]) -> Result<Vec<ExtractedPage>, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("transcript-{}-{stamp}.json", std::process::id()));
    let temporary = TemporaryOutput(path.clone());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let output = options
        .open(&path)
        .map_err(|e| format!("Could not create private extraction output: {e}"))?;
    let mut command = Command::new(executable);
    if executable == "py" {
        command.arg("-3");
    }
    let mut child = command
        .arg("-c")
        .arg(include_str!("pdf_adapter.py"))
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::from(output))
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            format!(
                "{executable}: local Python adapter unavailable; install requirements-parser.txt."
            )
        })?;
    let mut stdin = child.stdin.take().ok_or("Could not open adapter input")?;
    let input = bytes.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            let _ = writer.join();
            if !status.success() {
                return Err(
                    "Local coordinate adapter failed; check Python packages and PDF readability."
                        .to_string(),
                );
            }
            break;
        }
        let too_large = std::fs::metadata(&path)
            .map(|m| m.len() > 256 * 1024 * 1024)
            .unwrap_or(false);
        if started.elapsed() > Duration::from_secs(600) || too_large {
            let _ = child.kill();
            let _ = child.wait();
            let _ = writer.join();
            return Err("Coordinate extraction exceeded time/output limits. Split the document into smaller volumes.".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut text = String::new();
    File::open(&path)
        .map_err(|e| e.to_string())?
        .take(256 * 1024 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    let result: AdapterResult = serde_json::from_str(&text)
        .map_err(|e| format!("Invalid coordinate adapter output: {e}"))?;
    drop(temporary);
    if result.protocol_version != 1 || result.pages.is_empty() {
        return Err("Unsupported or empty coordinate adapter result".to_string());
    }
    if result
        .pages
        .iter()
        .enumerate()
        .any(|(i, p)| p.physical_page != (i + 1) as u32)
    {
        return Err(
            "Coordinate adapter returned a non-contiguous physical page inventory".to_string(),
        );
    }
    Ok(result.pages)
}
