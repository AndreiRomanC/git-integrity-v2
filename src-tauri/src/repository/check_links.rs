//! PR check Details links: validate first, then diagnose the OS handoff.
//! No network probes, shell interpolation, URL/query logging or automatic retry.
use super::{is_github_like_host, perf_log};
use std::{io, process::Command, time::Instant};

#[cfg(target_os = "macos")]
const LAUNCHER: &str = "open";
#[cfg(target_os = "windows")]
const LAUNCHER: &str = "explorer.exe";
#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
const LAUNCHER: &str = "xdg-open";

#[derive(Debug)]
enum Failure {
    Validation(&'static str),
    Launch(io::Error),
    Wait(io::Error),
    Exit(Option<i32>),
}

impl Failure {
    fn stage(&self) -> &'static str {
        match self { Self::Validation(_) => "validation", Self::Launch(_) => "launch", Self::Wait(_) => "wait", Self::Exit(_) => "exit" }
    }
    fn diagnostic(&self) -> String {
        match self {
            Self::Validation(reason) => format!("reason={reason}"),
            Self::Launch(error) | Self::Wait(error) => format!("reason=os_error kind={:?} os_code={:?}", error.kind(), error.raw_os_error()),
            Self::Exit(code) => format!("reason=non_success exit_code={code:?}"),
        }
    }
    fn message(&self) -> String {
        let number = |code: Option<i32>| code.map(|value| value.to_string()).unwrap_or_else(|| "unavailable".into());
        let detail = match self {
            Self::Validation(reason) => match *reason {
                "https_required" => "A check Details link must use HTTPS.",
                "untrusted_host" => "This check Details link is outside the allowed GitHub/corporate hosts.",
                "credentials_in_url" => "A check Details link must not contain embedded credentials.",
                _ => "The check Details URL is invalid.",
            }.to_string(),
            Self::Launch(error) => format!("Could not start {LAUNCHER} to open the check link (OS code {}, {:?}). Check the default browser and workstation restrictions.", number(error.raw_os_error()), error.kind()),
            Self::Wait(error) => format!("Could not confirm the result from {LAUNCHER} (OS code {}, {:?}). Browser opening is unconfirmed.", number(error.raw_os_error()), error.kind()),
            Self::Exit(code) => format!("{LAUNCHER} reported a non-success exit (code {}). Browser opening is unconfirmed; check the browser before retrying.", number(*code)),
        };
        // Stable stage prefix for the IPC diagnostic; no untrusted OS text/URL.
        format!("[check-link:{}] {detail}", self.stage())
    }
}

fn validate(url: &str) -> Result<(), Failure> {
    if !url.starts_with("https://") { return Err(Failure::Validation("https_required")); }
    if url.contains('\\') || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(Failure::Validation("malformed_url"));
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| Failure::Validation("malformed_url"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Failure::Validation("credentials_in_url"));
    }
    let host = parsed.host_str().ok_or(Failure::Validation("malformed_url"))?;
    if !(is_github_like_host(host) || host == "vitesco.io" || host.ends_with(".vitesco.io")) {
        return Err(Failure::Validation("untrusted_host"));
    }
    Ok(())
}

fn open_with(url: &str, launch: impl FnOnce(&str) -> Result<(), Failure>) -> Result<(), String> {
    let started = Instant::now();
    match validate(url).and_then(|()| launch(url)) {
        Ok(()) => {
            // Exit 0 confirms only the launcher handoff, not page load/sign-in.
            perf_log(&format!("pr_check_link: stage=exit outcome=accepted launcher={LAUNCHER} exit_code=0 browser_confirmation=unavailable no_url_logged=true"), started.elapsed());
            Ok(())
        }
        Err(error) => {
            perf_log(&format!("pr_check_link: stage={} outcome=failed launcher={LAUNCHER} {} no_url_logged=true", error.stage(), error.diagnostic()), started.elapsed());
            Err(error.message())
        }
    }
}

pub(super) fn open(url: &str) -> Result<(), String> {
    open_with(url, |url| {
        // A single literal argument, including query '&' and fragments. Never
        // use cmd /c, and never treat Explorer exit 1 as verified success.
        let mut child = Command::new(LAUNCHER).arg(url).spawn().map_err(Failure::Launch)?;
        let status = child.wait().map_err(Failure::Wait)?;
        if status.success() { Ok(()) } else { Err(Failure::Exit(status.code())) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_trusted_https_links_reach_the_launcher_unchanged() {
        for url in ["https://github.com/o/r/checks/1", "https://github.vitesco.io/o/r/checks?x=1&y=2#details", "https://vitesco.io", "https://ci.vitesco.io:8443/job/1?token=PRIVATE", "https://github.com?check=1"] {
            let mut calls = 0;
            open_with(url, |arg| { calls += 1; assert_eq!(arg, url); Ok(()) }).unwrap();
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn invalid_or_deceptive_links_never_launch() {
        for url in ["http://github.com/x", "file:///tmp/x", "javascript:alert(1)", "https://example.org/x", "https://notvitesco.io/x", "https://ci.vitesco.io.evil.test/x", "https://github.com:443@evil.test/x", "https://PRIVATE@github.com/x", "https://github.com\\@evil.test/x", "https://github.com/\nPRIVATE", "https://github.com:bad/x", "https://"] {
            let error = open_with(url, |_| panic!("invalid URLs must not launch")).unwrap_err();
            assert!(error.starts_with("[check-link:validation]"), "{error}");
            assert!(!error.contains("PRIVATE"));
        }
    }

    #[test]
    fn launch_wait_and_exit_failures_remain_distinct_without_raw_errors() {
        for failure in [Failure::Launch(io::Error::from_raw_os_error(2)), Failure::Wait(io::Error::other("PRIVATE_URL_TOKEN")), Failure::Exit(Some(1)), Failure::Exit(None)] {
            let stage = failure.stage();
            assert!(!failure.diagnostic().contains("PRIVATE"));
            let mut calls = 0;
            let error = open_with("https://github.com/?token=PRIVATE", |_| { calls += 1; Err(failure) }).unwrap_err();
            assert_eq!(calls, 1, "no blind retries");
            assert!(error.starts_with(&format!("[check-link:{stage}]")));
            assert!(!error.contains("PRIVATE"));
        }
    }
}
