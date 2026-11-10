//! Watch the native push itself. EOF on a pipe is not proof that Git is
//! alive (a credential helper can inherit a writer), nor that Git exited.
use super::*;
use std::io::{self, Read};
use std::process::{Child, Output};

enum Ready { Idle, Read(usize), #[cfg(windows)] Closed }
const CHUNK: usize = 16 * 1024;
const PIPE_GRACE: Duration = Duration::from_millis(500);

#[cfg(unix)]
fn ready(pipe: &impl std::os::fd::AsRawFd) -> io::Result<Ready> {
    let mut fd = libc::pollfd { fd: pipe.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: a single initialized pollfd for a live, exclusively read pipe.
    let n = unsafe { libc::poll(&mut fd, 1, 0) };
    if n < 0 { return Err(io::Error::last_os_error()); }
    if n == 0 { Ok(Ready::Idle) }
    else if fd.revents & (libc::POLLIN | libc::POLLHUP) != 0 { Ok(Ready::Read(CHUNK)) }
    else { Err(io::Error::other("pipe unavailable")) }
}

#[cfg(windows)]
fn ready(pipe: &impl std::os::windows::io::AsRawHandle) -> io::Result<Ready> {
    use windows_sys::Win32::{Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED}, System::Pipes::PeekNamedPipe};
    let mut available = 0;
    // SAFETY: a live pipe handle, no concurrent operations on this handle,
    // no data buffer, one writable DWORD. Never read an empty/open pipe.
    let ok = unsafe { PeekNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut(), 0,
        std::ptr::null_mut(), &mut available, std::ptr::null_mut()) };
    if ok == 0 {
        let err = io::Error::last_os_error();
        return match err.raw_os_error().map(|v| v as u32) {
            Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(Ready::Closed), _ => Err(err),
        };
    }
    Ok(if available == 0 { Ready::Idle } else { Ready::Read(CHUNK.min(available as usize)) })
}

#[cfg(unix)]
use std::os::fd::AsRawFd as PipeHandle;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle as PipeHandle;

enum Stream { Open, Eof, Error }
impl Stream {
    fn label(&self) -> &'static str { match self { Self::Open => "open", Self::Eof => "eof", Self::Error => "capture_error" } }
    fn open(&self) -> bool { matches!(self, Self::Open) }
}

fn pump(pipe: &mut (impl Read + PipeHandle), state: &mut Stream, buf: &mut [u8; CHUNK],
        trace: &PushTrace, name: &str, consume: impl FnOnce(&[u8])) -> bool {
    if !state.open() { return false; }
    let result = match ready(pipe) {
        Ok(Ready::Idle) => return false,
        #[cfg(windows)]
        Ok(Ready::Closed) => Ok(0),
        Ok(Ready::Read(n)) => pipe.read(&mut buf[..n]),
        Err(e) => Err(e),
    };
    match result {
        Ok(0) => *state = Stream::Eof,
        Ok(n) => { consume(&buf[..n]); return true; }
        Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) => return false,
        Err(_) => *state = Stream::Error,
    }
    trace.log(&format!("event=pipe stream={name} state={}", state.label()));
    false
}

pub(in crate::repository) fn wait(mut child: Child, trace: PushTrace, timeout: Duration, timeout_label: &str) -> Result<Output, String> {
    let start = Instant::now();
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let (mut os, mut es) = (Stream::Open, Stream::Open);
    let (mut stdout, mut capture) = (Vec::new(), Capture::default());
    let mut truncated = false;
    let mut buf = [0; CHUNK];
    let mut exited = None;
    let mut timeout_requested = false;
    let mut helper_open_at_timeout = false;
    let mut next_termination = timeout;
    let mut next_report = Duration::from_secs(30);
    trace.log("event=process_spawned");
    let status = loop {
        // Drain fairly even when stdout or Trace2 is noisy. No blocking pipe
        // reads and no detached reader threads that could outlive this push.
        let a = pump(&mut out, &mut os, &mut buf, &trace, "stdout", |bytes| {
            let n = bytes.len().min(RECENT_GIT_COMMAND_OUTPUT_LIMIT.saturating_sub(stdout.len()));
            stdout.extend_from_slice(&bytes[..n]); truncated |= n != bytes.len();
        });
        let b = pump(&mut err, &mut es, &mut buf, &trace, "stderr", |bytes| capture.feed(&trace, bytes));
        if exited.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    trace.log(&format!("event=git_exit code={} stdout={} stderr={}", status.code().map(|v| v.to_string()).unwrap_or_else(|| "signal".into()), os.label(), es.label()));
                    exited = Some((status, Instant::now()));
                }
                Ok(None) => (),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err("Cannot verify whether Git push exited. Check the destination branch before retrying; no automatic retry.".into()),
            }
        }
        if let Some((status, at)) = exited {
            if (!os.open() && !es.open()) || at.elapsed() >= PIPE_GRACE { break status; }
        } else {
            let elapsed = start.elapsed();
            if elapsed >= next_report {
                trace.log(&format!("event=waiting git=running stdout={} stderr={} {}", os.label(), es.label(), capture.summary.snapshot()));
                next_report = elapsed + Duration::from_secs(30);
            }
            if elapsed >= next_termination {
                trace.log(&format!("event=timeout git=running {}", capture.summary.snapshot()));
                // Never terminate a PID after observing its exit. Reuse the
                // original child's process-tree guard, not a PID from the UI.
                match process_control::kill_tree(&mut child) {
                    Ok(true) => {
                        timeout_requested = true;
                        helper_open_at_timeout = capture.summary.helper_observed_open();
                    }
                    Ok(false) => continue, // Exit won the race; inspect its status.
                    Err(_) => trace.log("event=termination_failed awaiting_git_exit=true"),
                }
                // Keep the existing repository lock until we observe exit.
                next_termination = elapsed + Duration::from_secs(30);
            }
        }
        if !a && !b { std::thread::sleep(Duration::from_millis(20)); }
    };
    if matches!(es, Stream::Eof) && !capture.line.is_empty() { capture.flush_line(&trace); }
    trace.log(&format!("event=capture_finished stdout={} stderr={} timeout_requested={} {}", os.label(), es.label(), timeout_requested, capture.summary.snapshot()));
    if truncated || !matches!(os, Stream::Eof) { stdout.extend_from_slice(b"\n[Git stdout capture incomplete.]\n"); }
    if timeout_requested {
        let reason = capture.summary.stderr_reason.unwrap_or("not observed");
        capture.finish(&trace, matches!(es, Stream::Eof));
        let helper = if helper_open_at_timeout { " Git had reported a credential helper starting, but no exit was observed for it." } else { "" };
        return Err(format!("Git push timed out after {timeout_label}.{helper} Last recognized Git error: {reason}. The remote result is unknown. Check the destination branch and commit before retrying. No automatic retry was performed."));
    }
    let stderr = capture.finish(&trace, matches!(es, Stream::Eof));
    Ok(Output { status, stdout, stderr })
}
