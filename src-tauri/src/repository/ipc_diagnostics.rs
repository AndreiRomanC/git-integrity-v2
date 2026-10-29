//! Correlation only: no new commands, repository reads or response changes.
use super::*;

pub(super) struct IpcTiming {
    request: Option<u64>,
    command: &'static str,
    started: Instant,
}

impl IpcTiming {
    pub(super) fn new(command: &'static str, request: Option<u64>) -> Self {
        let timing = Self { request, command, started: Instant::now() };
        timing.mark("backend_received");
        timing
    }

    pub(super) fn mark(&self, event: &'static str) {
        if let Some(request) = self.request {
            let unix_ms = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
            perf_log(&format!("ipc_delivery: request={request} command={} event={event} unix_ms={unix_ms}", self.command), self.started.elapsed());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn correlation_uses_only_id_command_phase_and_clock() {
        let (_, logs) = perf_logging::capture_for_test(|| {
            let timing = IpcTiming::new("refresh_status", Some(42));
            timing.mark("worker_finished");
            timing.mark("response_ready");
            IpcTiming::new("refresh_status", None).mark("response_ready");
        });
        assert_eq!(logs.len(), 3);
        assert!(logs.iter().all(|line| line.contains("request=42 command=refresh_status") && line.contains("unix_ms=")));
    }
}
