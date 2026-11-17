//! On-demand Settings reads. Account results never contain credentials and are
//! separate from Git commit identity. Existing GitHub/Polarion transports own
//! authentication; future token UI can use the same service + host boundary.
use super::*;

#[derive(Serialize)]
pub struct GithubServer {
    host: String,
    remote: String,
}
#[derive(Serialize, Debug, PartialEq)]
pub struct IdentityValue {
    value: String,
    scope: String,
}
#[derive(Serialize)]
pub struct LocalSettings {
    repository_path: Option<String>,
    github_servers: Vec<GithubServer>,
    name: Option<IdentityValue>,
    email: Option<IdentityValue>,
    identity_error: bool,
}
#[derive(Serialize)]
pub struct AccountCheck {
    service: &'static str,
    host: String,
    state: &'static str,
    method: &'static str,
    login: String,
    name: String,
    detail: String,
}
impl AccountCheck {
    fn result(host: &str, state: &'static str, method: &'static str, detail: &str) -> Self {
        Self {
            service: "github",
            host: host.into(),
            state,
            method,
            login: String::new(),
            name: String::new(),
            detail: detail.into(),
        }
    }
}

fn valid_host(host: &str) -> bool {
    is_github_like_host(host)
        && host.len() <= 253
        && host.split('.').all(|part| {
            !part.is_empty()
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}

fn github_servers(repo: &Repository) -> Result<Vec<GithubServer>, String> {
    let config = repo
        .config()
        .map_err(|_| "Cannot read repository configuration")?;
    let preferred = repo
        .head()
        .ok()
        .filter(|h| h.is_branch())
        .and_then(|h| h.shorthand().map(str::to_owned))
        .and_then(|branch| config.get_string(&format!("branch.{branch}.remote")).ok())
        .unwrap_or_else(|| "origin".into());
    let names = repo
        .remotes()
        .map_err(|_| "Cannot read configured remotes")?;
    let mut names: Vec<_> = names.iter().flatten().collect();
    names.sort_by_key(|name| (*name != preferred, *name != "origin", *name));
    let mut hosts = HashSet::new();
    Ok(names
        .into_iter()
        .filter_map(|name| {
            let remote = repo.find_remote(name).ok()?;
            let parsed = parse_github_repo(remote.url()?)?;
            (valid_host(&parsed.host) && hosts.insert(parsed.host.clone())).then(|| GithubServer {
                host: parsed.host,
                remote: name.into(),
            })
        })
        .collect())
}

fn parse_identity(raw: &[u8]) -> Option<IdentityValue> {
    let text = std::str::from_utf8(raw)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    let (scope, value) = text.split_once('\t')?;
    if !matches!(
        scope,
        "system" | "global" | "local" | "worktree" | "command"
    ) {
        return None;
    }
    Some(IdentityValue {
        value: value.to_owned(),
        scope: scope.into(),
    })
}
fn identity(repository: Option<&str>, key: &str) -> Result<Option<IdentityValue>, ()> {
    let mut cmd = Command::new("git");
    if let Some(path) = repository {
        cmd.arg("-C").arg(path);
    }
    cmd.args(["config", "--show-scope", "--includes"]);
    if repository.is_none() {
        cmd.arg("--global");
    }
    cmd.args(["--get", key])
        .stdin(std::process::Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0");
    let output = run_with_timeout_labeled(
        cmd,
        Duration::from_secs(5),
        "Settings Git identity",
        "5 seconds",
    )
    .map_err(|_| ())?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err(());
    }
    parse_identity(&output.stdout).map(Some).ok_or(())
}
#[tauri::command]
pub async fn settings_local(repository_path: Option<String>) -> Result<LocalSettings, String> {
    off_main_thread(move || {
        let github_servers = if let Some(path) = &repository_path {
            validate_path(path)?;
            github_servers(&internal_repository(path)?)?
        } else {
            Vec::new()
        };
        let name = identity(repository_path.as_deref(), "user.name");
        let email = identity(repository_path.as_deref(), "user.email");
        let identity_error = name.is_err() || email.is_err();
        Ok(LocalSettings {
            repository_path,
            github_servers,
            name: name.ok().flatten(),
            email: email.ok().flatten(),
            identity_error,
        })
    })
    .await
}

fn configured_github_repo(repository_path: &str, host: &str) -> Result<GitHubRepo, String> {
    validate_path(repository_path)?;
    if !valid_host(host) {
        return Err("Invalid GitHub host".into());
    }
    let repo = internal_repository(repository_path)?;
    for name in repo
        .remotes()
        .map_err(|_| "Cannot read remotes")?
        .iter()
        .flatten()
    {
        if let Some(parsed) = repo
            .find_remote(name)
            .ok()
            .and_then(|r| r.url().and_then(parse_github_repo))
        {
            if parsed.host == host {
                return Ok(parsed);
            }
        }
    }
    Err(
        "This GitHub host is no longer configured for the selected repository. Reopen Settings."
            .into(),
    )
}
fn user_endpoint(host: &str) -> String {
    if host == "github.com" {
        "https://api.github.com/user".into()
    } else {
        format!("https://{host}/api/v3/user")
    }
}
fn account_from_payload(
    host: &str,
    method: &'static str,
    payload: &serde_json::Value,
) -> AccountCheck {
    let login = payload.get("login").and_then(|v| v.as_str()).unwrap_or("");
    if login.is_empty() || login.len() > 200 || login.chars().any(char::is_control) {
        return AccountCheck::result(
            host,
            "failed",
            method,
            "GitHub did not return a signed-in user.",
        );
    }
    let mut result = AccountCheck::result(
        host,
        "connected",
        method,
        "Account verified. Repository access and Push permissions are checked separately.",
    );
    result.login = login.into();
    result.name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect();
    result
}
fn check_github(
    repository_path: &str,
    host: &str,
    interactive: bool,
) -> Result<AccountCheck, String> {
    let repo = configured_github_repo(repository_path, host)?;
    let session = github_session_token(host);
    let environment = github_token_from_environment(host).is_some();
    if !interactive
        && session.is_none()
        && !environment
        && gh_cli_available()
        && gh_cli_authenticated(host)
    {
        let mut command = Command::new("gh");
        command
            .args(["api", "--hostname", host, "--method", "GET", "user"])
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(std::process::Stdio::null());
        // Never return CLI stderr: helpers can include credentials or URLs.
        let output = run_with_timeout_labeled(
            command,
            Duration::from_secs(20),
            "Settings GitHub account",
            "20 seconds",
        );
        return Ok(match output {
            Ok(output) if output.status.success() => serde_json::from_slice(&output.stdout)
                .map(|payload| account_from_payload(host, "github_cli", &payload))
                .unwrap_or_else(|_| AccountCheck::result(host, "failed", "github_cli", "GitHub returned an unreadable account response.")),
            _ => AccountCheck::result(host, "failed", "github_cli", "GitHub CLI could not verify the account. Check network access and sign-in for this host."),
        });
    }
    let method = if session.is_some() {
        "session"
    } else if environment && !interactive {
        "environment_token"
    } else {
        "git_credentials"
    };
    let api = match GitHubGraphqlClient::discover_with_auth(
        repository_path,
        &repo,
        interactive,
        session,
    ) {
        Ok(api) => api,
        Err(_) => {
            return Ok(AccountCheck::result(
                host,
                "sign_in_required",
                method,
                if interactive {
                    "Sign-in did not complete. Check the Git credential manager or browser window, then retry."
                } else {
                    "No usable API credential was found for this host. Use Sign in, then check again."
                },
            ))
        }
    };
    let Some(token) = api.token.as_deref() else {
        return Ok(AccountCheck::result(
            host,
            "sign_in_required",
            method,
            "An API credential is required.",
        ));
    };
    let response = match api
        .http
        .get(user_endpoint(host))
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .send()
    {
        Ok(response) => response,
        Err(_) => {
            return Ok(AccountCheck::result(
                host,
                "failed",
                method,
                "Cannot reach GitHub. Check the company network/VPN and retry.",
            ))
        }
    };
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        forget_github_session_token(host);
        return Ok(AccountCheck::result(
            host,
            "sign_in_required",
            method,
            "GitHub rejected the credential. Sign in and retry.",
        ));
    }
    if !status.is_success() {
        return Ok(AccountCheck::result(
            host,
            "failed",
            method,
            &format!(
                "GitHub account check returned HTTP {}. Access could not be verified.",
                status.as_u16()
            ),
        ));
    }
    let payload = match response.json::<serde_json::Value>() {
        Ok(payload) => payload,
        Err(_) => {
            return Ok(AccountCheck::result(
                host,
                "failed",
                method,
                "GitHub returned an unreadable account response.",
            ))
        }
    };
    let result = account_from_payload(host, method, &payload);
    if result.state == "connected" {
        if let Some(credential) = api.credential_to_approve {
            remember_github_session_token(host, credential.password.clone());
            approve_git_credential_in_background(repository_path.into(), repo, credential);
        }
    }
    Ok(result)
}
#[tauri::command]
pub async fn settings_check_github(
    repository_path: String,
    host: String,
    interactive: bool,
) -> Result<AccountCheck, String> {
    off_main_thread(move || {
        let started = Instant::now();
        let result = check_github(&repository_path, &host, interactive);
        if let Ok(result) = &result {
            perf_log(&format!("settings: service=github check={} method={} interactive={interactive} no_credentials_logged=true", result.state, result.method), started.elapsed());
        }
        result
    }).await
}

#[derive(Serialize)]
pub struct Diagnostics {
    version: String,
    build: String,
    platform: String,
    git_version: String,
    git_path: String,
    log_path: String,
}
#[tauri::command]
pub async fn settings_diagnostics() -> Result<Diagnostics, String> {
    off_main_thread(move || {
        let mut command = Command::new("git");
        command.arg("--version").stdin(std::process::Stdio::null());
        let version = run_with_timeout_labeled(
            command,
            Duration::from_secs(5),
            "Settings Git version",
            "5 seconds",
        )
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|| "Git unavailable".into());
        Ok(Diagnostics {
            version: env!("CARGO_PKG_VERSION").into(),
            build: build_info(),
            platform: format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
            git_version: version,
            git_path: git_executable_for_log(),
            log_path: perf_log_path().to_string_lossy().into_owned(),
        })
    })
    .await
}
#[tauri::command]
pub async fn settings_open_logs() -> Result<(), String> {
    off_main_thread(move || {
        let log = perf_log_path();
        let folder = log.parent().ok_or("Log folder unavailable")?;
        #[cfg(target_os = "macos")]
        let mut command = Command::new("open");
        #[cfg(target_os = "windows")]
        let mut command = Command::new("explorer.exe");
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        let mut command = Command::new("xdg-open");
        command.arg(folder).stdin(std::process::Stdio::null());
        let out = run_with_timeout_labeled(
            command,
            Duration::from_secs(5),
            "Open logs folder",
            "5 seconds",
        )?;
        if out.status.success() {
            Ok(())
        } else {
            Err("Log folder opening could not be confirmed. Use the path shown above.".into())
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_identity_scope_is_per_value_and_preserves_unicode() {
        assert_eq!(
            parse_identity(b"local\tAndrei Roman\n"),
            Some(IdentityValue {
                value: "Andrei Roman".into(),
                scope: "local".into()
            })
        );
        assert_eq!(
            parse_identity("global\tȘtefan\n".as_bytes()).unwrap().value,
            "Ștefan"
        );
        assert!(parse_identity(b"invalid\tvalue").is_none());
    }
    #[test]
    fn settings_identity_reads_effective_included_config_without_changing_repository() {
        let path = std::env::temp_dir().join(format!(
            "gdd-settings-identity-{}-{}",
            std::process::id(),
            UNIX_EPOCH.elapsed().unwrap().as_nanos()
        ));
        let repo = Repository::init(&path).unwrap();
        let included = path.join("identity.inc");
        fs::write(
            &included,
            "[user]\nname = Included Author\nemail = included@example.test\n",
        )
        .unwrap();
        repo.config()
            .unwrap()
            .set_str("include.path", included.to_str().unwrap())
            .unwrap();
        let config_path = repo.path().join("config");
        let before = fs::read(&config_path).unwrap();
        let result = identity(path.to_str(), "user.name").unwrap().unwrap();
        assert_eq!(result.value, "Included Author");
        assert_eq!(result.scope, "local");
        assert_eq!(
            identity(path.to_str(), "user.email")
                .unwrap()
                .unwrap()
                .value,
            "included@example.test"
        );
        assert_eq!(fs::read(&config_path).unwrap(), before);
        assert!(!repo.path().join("index").exists());
        drop(repo);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn settings_user_endpoint_is_host_specific() {
        assert_eq!(user_endpoint("github.com"), "https://api.github.com/user");
        assert_eq!(
            user_endpoint("github.vitesco.io"),
            "https://github.vitesco.io/api/v3/user"
        );
        for host in [
            "github.com@other",
            "github.x/path",
            "github.x:8443",
            "github.x\n",
            "github..x",
        ] {
            assert!(!valid_host(host));
        }
    }
    #[test]
    fn settings_account_response_only_exposes_identity_not_tokens() {
        let payload = serde_json::json!({"login":"octocat", "name":"Andrei", "token":"SECRET", "email":"private@example.com"});
        let result = account_from_payload("github.com", "session", &payload);
        assert_eq!(result.state, "connected");
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("SECRET") && !json.contains("private@example.com"));
        assert_eq!(
            account_from_payload(
                "github.com",
                "session",
                &serde_json::json!({"name":"Andrei"})
            )
            .state,
            "failed"
        );
    }
    #[test]
    fn settings_deduplicates_hosts_and_prefers_origin_without_exposing_urls() {
        let path = std::env::temp_dir().join(format!(
            "gdd-settings-{}-{}",
            std::process::id(),
            UNIX_EPOCH.elapsed().unwrap().as_nanos()
        ));
        let repo = Repository::init(&path).unwrap();
        repo.remote("origin", "https://secret@github.com/org/repo.git")
            .unwrap();
        repo.remote("enterprise", "git@github.vitesco.io:eng/module.git")
            .unwrap();
        repo.remote("duplicate", "https://github.com/another/repo.git")
            .unwrap();
        repo.remote("unrelated", "https://gitlab.com/o/r.git")
            .unwrap();
        let result = github_servers(&repo).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].host, "github.com");
        assert_eq!(result[0].remote, "origin");
        assert!(!serde_json::to_string(&result).unwrap().contains("secret"));
        assert!(configured_github_repo(path.to_str().unwrap(), "github.other.io").is_err());
        drop(repo);
        fs::remove_dir_all(path).unwrap();
    }
}
