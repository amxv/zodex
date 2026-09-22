use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;

use anyhow::{Context, Result};
#[cfg(not(unix))]
use tokio::io::AsyncReadExt as _;
use tokio::sync::Notify;

use crate::invocation::InvocationContext;

use super::{SessionOutputChunk, SessionOutputCompletion, SessionOutputObserver};

const SPILLED_OUTPUT_PREVIEW_MAX_BYTES: usize = 20 * 1024;
const OUTPUT_SPILL_MAX_BYTES: usize = 16 * 1024 * 1024;
const OUTPUT_SPILL_STALE_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Default)]
struct StreamingUtf8Decoder {
    pending: Vec<u8>,
}

impl StreamingUtf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        let mut joined = Vec::with_capacity(self.pending.len().saturating_add(bytes.len()));
        joined.append(&mut self.pending);
        joined.extend_from_slice(bytes);

        let mut output = String::with_capacity(joined.len());
        let mut remaining = joined.as_slice();
        while !remaining.is_empty() {
            match std::str::from_utf8(remaining) {
                Ok(valid) => {
                    output.push_str(valid);
                    break;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    if valid_up_to > 0 {
                        output.push_str(
                            std::str::from_utf8(&remaining[..valid_up_to])
                                .expect("UTF-8 validator reported an invalid valid prefix"),
                        );
                    }
                    match error.error_len() {
                        Some(invalid_len) => {
                            output.push('\u{fffd}');
                            remaining = &remaining[valid_up_to + invalid_len..];
                        }
                        None => {
                            self.pending.extend_from_slice(&remaining[valid_up_to..]);
                            debug_assert!(self.pending.len() <= 3);
                            break;
                        }
                    }
                }
            }
        }
        output
    }

    fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        self.pending.clear();
        "\u{fffd}".to_string()
    }
}

#[derive(Debug)]
struct OutputState {
    text: String,
    dropped_bytes: usize,
    total_chars: usize,
    newline_count: usize,
    ends_with_newline: bool,
    spill: Option<OutputSpill>,
    spill_failed: bool,
}

#[derive(Debug)]
struct OutputSpill {
    path: PathBuf,
    file: File,
    bytes_written: usize,
    truncated: bool,
}

#[derive(Debug, Clone)]
pub(super) struct OutputSnapshot {
    pub text: String,
    pub output_file: Option<String>,
    pub output_chars: Option<usize>,
    pub output_lines: Option<usize>,
    pub output_file_truncated: Option<bool>,
}

#[derive(Debug)]
pub(super) struct OutputBuffer {
    inner: StdMutex<OutputState>,
    max_chars: usize,
    spill_path: PathBuf,
    reader_done: AtomicBool,
    reader_done_notify: Notify,
}

impl OutputBuffer {
    pub(super) fn new(max_chars: usize, spill_path: PathBuf) -> Self {
        Self {
            inner: StdMutex::new(OutputState {
                text: String::new(),
                dropped_bytes: 0,
                total_chars: 0,
                newline_count: 0,
                ends_with_newline: false,
                spill: None,
                spill_failed: false,
            }),
            max_chars,
            spill_path,
            reader_done: AtomicBool::new(false),
            reader_done_notify: Notify::new(),
        }
    }

    pub(super) fn append(&self, chunk: &str) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.total_chars = state.total_chars.saturating_add(chunk.chars().count());
        state.newline_count = state
            .newline_count
            .saturating_add(chunk.bytes().filter(|byte| *byte == b'\n').count());
        if !chunk.is_empty() {
            state.ends_with_newline = chunk.ends_with('\n');
        }

        let would_overflow = state.text.len().saturating_add(chunk.len()) > self.max_chars;
        if would_overflow && state.spill.is_none() && !state.spill_failed {
            match start_output_spill(&self.spill_path, &state.text, chunk) {
                Ok(spill) => state.spill = Some(spill),
                Err(_) => state.spill_failed = true,
            }
        } else if let Some(spill) = state.spill.as_mut()
            && append_to_spill(spill, chunk).is_err()
        {
            let failed = state.spill.take().expect("spill existed");
            let _ = std::fs::remove_file(failed.path);
            state.spill_failed = true;
        }

        state.text.push_str(chunk);

        if state.text.len() <= self.max_chars {
            return;
        }

        let overflow = state.text.len() - self.max_chars;
        let cut = next_char_boundary(&state.text, overflow);
        state.text.drain(..cut);
        state.dropped_bytes += cut;
    }

    pub(super) fn snapshot(&self) -> OutputSnapshot {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let output_lines = if state.total_chars == 0 {
            0
        } else {
            state
                .newline_count
                .saturating_add(usize::from(!state.ends_with_newline))
        };
        let output_file = state
            .spill
            .as_ref()
            .map(|spill| spill.path.display().to_string());
        let output_file_truncated = state.spill.as_ref().map(|spill| spill.truncated);
        let text = match output_file.as_deref() {
            Some(path) => {
                let requested_start = state
                    .text
                    .len()
                    .saturating_sub(SPILLED_OUTPUT_PREVIEW_MAX_BYTES);
                let preview_start = next_char_boundary(&state.text, requested_start);
                if output_file_truncated == Some(true) {
                    format!(
                        "[output was very large; first {cap} bytes saved to {path} ({chars} characters, {lines} lines total); saved file is capped, showing tail preview]\n{}",
                        &state.text[preview_start..],
                        cap = OUTPUT_SPILL_MAX_BYTES,
                        chars = state.total_chars,
                        lines = output_lines,
                    )
                } else {
                    format!(
                        "[full output saved to {path} ({chars} characters, {lines} lines); showing tail preview]\n{}",
                        &state.text[preview_start..],
                        chars = state.total_chars,
                        lines = output_lines,
                    )
                }
            }
            None if state.dropped_bytes == 0 => state.text.clone(),
            None => format!(
                "[... {} bytes truncated ...]\n{}",
                state.dropped_bytes, state.text
            ),
        };
        OutputSnapshot {
            text,
            output_chars: output_file.as_ref().map(|_| state.total_chars),
            output_lines: output_file.as_ref().map(|_| output_lines),
            output_file_truncated,
            output_file,
        }
    }

    pub(super) fn mark_reader_done(&self) {
        self.reader_done.store(true, Ordering::Release);
        self.reader_done_notify.notify_waiters();
    }

    pub(super) async fn wait_for_reader_done(&self, timeout: Duration) {
        if self.reader_done.load(Ordering::Acquire) {
            return;
        }

        let notified = self.reader_done_notify.notified();
        if self.reader_done.load(Ordering::Acquire) {
            return;
        }

        let _ = tokio::time::timeout(timeout, notified).await;
    }
}

fn start_output_spill(path: &PathBuf, prefix: &str, chunk: &str) -> std::io::Result<OutputSpill> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    let mut spill = OutputSpill {
        path: path.clone(),
        file,
        bytes_written: 0,
        truncated: false,
    };
    if let Err(error) =
        append_to_spill(&mut spill, prefix).and_then(|()| append_to_spill(&mut spill, chunk))
    {
        drop(spill.file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(spill)
}

fn append_to_spill(spill: &mut OutputSpill, text: &str) -> std::io::Result<()> {
    if spill.truncated || text.is_empty() {
        return Ok(());
    }
    let remaining = OUTPUT_SPILL_MAX_BYTES.saturating_sub(spill.bytes_written);
    if remaining == 0 {
        spill.truncated = true;
        return Ok(());
    }
    let mut write_len = text.len().min(remaining);
    while write_len > 0 && !text.is_char_boundary(write_len) {
        write_len -= 1;
    }
    if write_len > 0 {
        spill.file.write_all(&text.as_bytes()[..write_len])?;
        spill.bytes_written = spill.bytes_written.saturating_add(write_len);
    }
    if write_len < text.len() {
        spill.truncated = true;
    }
    Ok(())
}

pub(super) fn spill_path_for_session(session_handle: &str) -> PathBuf {
    static CLEANUP: Once = Once::new();
    let dir = std::env::temp_dir().join("zodex-output");
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    CLEANUP.call_once(|| cleanup_stale_spills(&dir));
    dir.join(format!("zodex-output-{session_handle}.log"))
}

fn cleanup_stale_spills(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("zodex-output-") && name.ends_with(".log"))
        {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= OUTPUT_SPILL_STALE_AGE);
        if stale {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(super) fn spawn_reader(
    mut reader: std::fs::File,
    output: Arc<OutputBuffer>,
    observer: Arc<dyn SessionOutputObserver>,
    internal_session_id: u64,
    session_handle: Arc<str>,
    invocation: InvocationContext,
) -> Result<()> {
    std::thread::Builder::new()
        .name("zodex-pty-reader".to_string())
        .spawn(move || {
            let mut buf = [0_u8; 8192];
            let mut sequence = 0_u64;
            let mut decoder = StreamingUtf8Decoder::default();
            loop {
                let read = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };

                let chunk = decoder.push(&buf[..read]);
                observer.observe_output(SessionOutputChunk {
                    internal_session_id,
                    session_handle: session_handle.clone(),
                    invocation: invocation.clone(),
                    sequence,
                    text: chunk.clone(),
                });
                sequence = sequence.saturating_add(1);
                output.append(&chunk);
            }
            let final_chunk = decoder.finish();
            if !final_chunk.is_empty() {
                observer.observe_output(SessionOutputChunk {
                    internal_session_id,
                    session_handle: session_handle.clone(),
                    invocation: invocation.clone(),
                    sequence,
                    text: final_chunk.clone(),
                });
                output.append(&final_chunk);
            }
            observer.observe_output_complete(SessionOutputCompletion {
                internal_session_id,
                session_handle,
                invocation,
            });
            output.mark_reader_done();
        })
        .context("failed to start PTY output reader thread")?;
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn spawn_pipe_readers(
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
    output: Arc<OutputBuffer>,
    observer: Arc<dyn SessionOutputObserver>,
    internal_session_id: u64,
    session_handle: Arc<str>,
    invocation: InvocationContext,
) {
    tokio::spawn(async move {
        let mut stdout_open = true;
        let mut stderr_open = true;
        let mut stdout_decoder = StreamingUtf8Decoder::default();
        let mut stderr_decoder = StreamingUtf8Decoder::default();
        let mut stdout_buffer = [0_u8; 8192];
        let mut stderr_buffer = [0_u8; 8192];
        let mut sequence = 0_u64;

        while stdout_open || stderr_open {
            tokio::select! {
                result = stdout.read(&mut stdout_buffer), if stdout_open => {
                    match result {
                        Ok(0) | Err(_) => stdout_open = false,
                        Ok(read) => {
                            let chunk = stdout_decoder.push(&stdout_buffer[..read]);
                            observe_pipe_chunk(
                                &output,
                                observer.as_ref(),
                                internal_session_id,
                                &session_handle,
                                &invocation,
                                &mut sequence,
                                chunk,
                            );
                        }
                    }
                }
                result = stderr.read(&mut stderr_buffer), if stderr_open => {
                    match result {
                        Ok(0) | Err(_) => stderr_open = false,
                        Ok(read) => {
                            let chunk = stderr_decoder.push(&stderr_buffer[..read]);
                            observe_pipe_chunk(
                                &output,
                                observer.as_ref(),
                                internal_session_id,
                                &session_handle,
                                &invocation,
                                &mut sequence,
                                chunk,
                            );
                        }
                    }
                }
            }
        }

        for final_chunk in [stdout_decoder.finish(), stderr_decoder.finish()] {
            observe_pipe_chunk(
                &output,
                observer.as_ref(),
                internal_session_id,
                &session_handle,
                &invocation,
                &mut sequence,
                final_chunk,
            );
        }
        observer.observe_output_complete(SessionOutputCompletion {
            internal_session_id,
            session_handle,
            invocation,
        });
        output.mark_reader_done();
    });
}

#[cfg(not(unix))]
fn observe_pipe_chunk(
    output: &OutputBuffer,
    observer: &dyn SessionOutputObserver,
    internal_session_id: u64,
    session_handle: &Arc<str>,
    invocation: &InvocationContext,
    sequence: &mut u64,
    chunk: String,
) {
    if chunk.is_empty() {
        return;
    }
    observer.observe_output(SessionOutputChunk {
        internal_session_id,
        session_handle: session_handle.clone(),
        invocation: invocation.clone(),
        sequence: *sequence,
        text: chunk.clone(),
    });
    *sequence = (*sequence).saturating_add(1);
    output.append(&chunk);
}

pub(super) fn next_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }

    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{OUTPUT_SPILL_MAX_BYTES, OutputBuffer, StreamingUtf8Decoder};

    #[test]
    fn streaming_utf8_decoder_preserves_valid_scalars_across_every_byte_boundary() {
        let text = "ascii-é-漢-🙂-done";
        let bytes = text.as_bytes();
        for split in 0..=bytes.len() {
            let mut decoder = StreamingUtf8Decoder::default();
            let decoded = [
                decoder.push(&bytes[..split]),
                decoder.push(&bytes[split..]),
                decoder.finish(),
            ]
            .concat();
            assert_eq!(decoded, text, "split at byte {split}");
        }

        let mut decoder = StreamingUtf8Decoder::default();
        let mut decoded = String::new();
        for byte in bytes {
            decoded.push_str(&decoder.push(std::slice::from_ref(byte)));
        }
        decoded.push_str(&decoder.finish());
        assert_eq!(decoded, text);
    }

    #[test]
    fn streaming_utf8_decoder_keeps_lossy_invalid_and_incomplete_eof_behavior() {
        let mut invalid = StreamingUtf8Decoder::default();
        let decoded = [invalid.push(b"a\xffb"), invalid.finish()].concat();
        assert_eq!(decoded, "a\u{fffd}b");

        let mut incomplete = StreamingUtf8Decoder::default();
        assert_eq!(incomplete.push(b"a\xf0\x9f"), "a");
        assert_eq!(incomplete.finish(), "\u{fffd}");
    }

    #[test]
    fn oversized_spill_file_is_bounded_and_reports_total_output_separately() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("oversized.log");
        let output = OutputBuffer::new(64, path.clone());
        let total_chars = OUTPUT_SPILL_MAX_BYTES + 1024;
        output.append(&"x".repeat(total_chars));

        let snapshot = output.snapshot();
        assert_eq!(
            snapshot.output_file.as_deref(),
            Some(path.to_str().unwrap())
        );
        assert_eq!(snapshot.output_chars, Some(total_chars));
        assert_eq!(snapshot.output_lines, Some(1));
        assert_eq!(snapshot.output_file_truncated, Some(true));
        assert!(snapshot.text.contains("saved file is capped"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            OUTPUT_SPILL_MAX_BYTES as u64
        );
    }
}
