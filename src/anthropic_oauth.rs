//! Anthropic OAuth for Claude Code subscription providers.
//!
//! Stoke never sees an Anthropic API key here: the operator logs in once with
//! the official Claude Code OAuth app (PKCE S256), and Stoke keeps the
//! resulting tokens in `~/.stoke/anthropic_oauth.json`. The store is a secret
//! like any private key: created 0600 inside a 0700 directory, refreshed
//! atomically, and never echoed into logs, Debug output, or error strings.

use base64::Engine;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;

/// The public client id baked into the official Claude Code CLI. It is a
/// public application identifier (like every VS Code install sharing one
/// GitHub OAuth client id), not a secret.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// The exact upstream host a claude_subscription provider may talk to.
pub const ANTHROPIC_API_HOST: &str = "api.anthropic.com";
/// The scopes Claude Code's interactive login requests. The OAuth server
/// rejects unknown scopes with "Invalid OAuth Request", so this tracks the
/// set actually issued to current Claude Code clients (anthropics/
/// claude-code#54502: granted scopes are these five user:* scopes;
/// org:create_api_key is requested by the CLI but silently dropped for
/// subscription accounts, so requesting it is optional — and the older
/// org:custom_attributes / claude_code scopes now 400 as unknown).
pub const OAUTH_SCOPES: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// Refresh when the access token is within this window of expiring.
pub const EXPIRY_SKEW_SECS: i64 = 60;
/// How long `login()` waits for the browser to come back to the local callback.
const CALLBACK_TIMEOUT_SECS: u64 = 600;

pub static OAUTH_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("OAuth HTTP client must build")
});

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds after which the access token must not be used.
    pub expires_at: i64,
    pub created_at: i64,
}

// Access tokens are credentials. A `Debug` print of a token must never leak
// them into a log file, so they are redacted at the type level.
impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("created_at", &self.created_at)
            .finish()
    }
}

pub struct TokenStore {
    path: PathBuf,
    token_url: &'static str,
    state: tokio::sync::Mutex<Option<TokenSet>>,
}

/// Login state for `stoke login-claude --status`. Carries no token material —
/// only whether the operator is logged in and when the access token expires.
pub struct LoginStatus {
    pub logged_in: bool,
    /// Unix seconds at which the stored access token expires, when logged in.
    pub expires_at: Option<i64>,
}

impl Default for TokenStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenStore {
    pub fn new() -> Self {
        Self {
            path: default_store_path(),
            token_url: TOKEN_URL,
            state: tokio::sync::Mutex::new(None),
        }
    }

    /// Test/alternate-location constructor: same rules, different file.
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            path,
            token_url: TOKEN_URL,
            state: tokio::sync::Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_token_url(path: PathBuf, token_url: &'static str) -> Self {
        Self {
            path,
            token_url,
            state: tokio::sync::Mutex::new(None),
        }
    }

    /// The token file is cached in memory; anything not yet in memory is read
    /// from disk exactly once. A missing, malformed, or wrongly-permissioned
    /// store means "not logged in" — never a panic.
    async fn ensure_loaded(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if state.is_some() {
            return Ok(());
        }
        match read_store(&self.path) {
            Some(Ok(tokens)) => {
                *state = Some(tokens);
                Ok(())
            }
            Some(Err(reason)) => Err(reason),
            None => Err(format!(
                "not logged in: no usable Anthropic OAuth token store at {}",
                self.path.display()
            )),
        }
    }

    /// Persist new tokens: memory and disk move together, under one lock.
    async fn store(&self, tokens: TokenSet) -> Result<(), String> {
        let mut state = self.state.lock().await;
        write_store(&self.path, &tokens)?;
        *state = Some(tokens);
        Ok(())
    }

    /// Return a usable access token: the cached one if it is not near expiry,
    /// otherwise exactly one refresh attempt. Concurrent callers serialize on
    /// the state lock, so a token nearing expiry produces one refresh, not a
    /// stampede against Anthropic's token endpoint.
    pub async fn get_valid_access_token(&self) -> Result<String, String> {
        self.ensure_loaded().await?;
        let now = chrono::Utc::now().timestamp();
        {
            let state = self.state.lock().await;
            if let Some(tokens) = state.as_ref() {
                if tokens.expires_at > now + EXPIRY_SKEW_SECS {
                    return Ok(tokens.access_token.clone());
                }
            }
        }
        self.refresh_if_expired().await
    }

    /// Force a refresh (grant_type=refresh_token) and atomically rewrite the
    /// store. On any failure the previous tokens stay in place and the error
    /// is returned — never a panic, never a truncated store.
    pub async fn refresh(&self) -> Result<(), String> {
        self.ensure_loaded().await?;
        self.refresh_if_expired().await.map(|_| ())
    }

    /// The refresh path. Holding the state lock across the HTTP call both
    /// serializes concurrent refreshes and lets the second caller re-check
    /// freshness after the first one finishes (double-checked locking).
    async fn refresh_if_expired(&self) -> Result<String, String> {
        let mut state = self.state.lock().await;
        let current = match state.as_ref() {
            Some(tokens) => tokens.clone(),
            None => return Err("not logged in: no Anthropic OAuth tokens to refresh".to_string()),
        };
        // Someone else may have refreshed while we waited for the lock.
        if current.expires_at > chrono::Utc::now().timestamp() + EXPIRY_SKEW_SECS {
            return Ok(current.access_token);
        }
        if current.refresh_token.is_empty() {
            return Err(
                "stored Anthropic OAuth tokens have no refresh token; log in again".to_string(),
            );
        }
        let payload = json!({
            "grant_type": "refresh_token",
            "refresh_token": current.refresh_token,
            "client_id": CLIENT_ID,
        });
        let fresh = match post_token(&payload, self.token_url).await {
            Ok(tokens) => tokens,
            Err(reason) => {
                // The old tokens remain stored and in memory; the caller can
                // retry or fall back to an interactive login.
                return Err(format!("Anthropic OAuth refresh failed: {reason}"));
            }
        };
        let mut fresh = fresh;
        if fresh.refresh_token.is_empty() {
            // A refresh response that omits refresh_token means "keep using
            // the one you have" (token rotation is opt-in per response).
            fresh.refresh_token = current.refresh_token;
        }
        if fresh.expires_at <= chrono::Utc::now().timestamp() {
            return Err("Anthropic OAuth refresh returned an already-expired token".to_string());
        }
        write_store(&self.path, &fresh)?;
        *state = Some(fresh.clone());
        Ok(fresh.access_token)
    }

    /// Interactive login: open the operator's browser at the authorize URL,
    /// catch the redirect on a loopback listener, exchange the code.
    pub async fn login(&self) -> Result<(), String> {
        let (listener, verifier, url) = self.begin_login()?;
        open_browser(&url)?;
        self.finish_login(listener, &verifier).await
    }

    /// Bind the loopback callback listener and build the authorize URL without
    /// opening a browser. The `stoke login-claude` CLI pairs this with
    /// `finish_login` so it can print the URL before a browser is launched.
    pub fn begin_login(&self) -> Result<(std::net::TcpListener, String, String), String> {
        let (verifier, challenge) = pkce_pair();
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("could not bind local OAuth callback listener: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("could not read callback port: {e}"))?
            .port();
        let redirect_uri = format!("http://localhost:{port}/callback");
        let url = authorization_url(&verifier, &challenge, &redirect_uri);
        Ok((listener, verifier, url))
    }

    /// Complete a login whose callback listener is already bound: wait for the
    /// browser redirect, verify the state against the PKCE verifier, exchange
    /// the code, and persist the tokens. `verifier` doubles as the OAuth
    /// `state` — a mismatch is a cross-site forgery attempt or a stale tab.
    pub async fn finish_login(
        &self,
        listener: std::net::TcpListener,
        verifier: &str,
    ) -> Result<(), String> {
        let port = listener
            .local_addr()
            .map_err(|e| format!("could not read callback port: {e}"))?
            .port();
        let redirect_uri = format!("http://localhost:{port}/callback");
        let (code, state) = wait_for_callback(listener)?;
        if state != verifier {
            return Err("OAuth state mismatch — refusing to exchange the code".to_string());
        }
        let payload = json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": redirect_uri,
            "client_id": CLIENT_ID,
            "code_verifier": verifier,
            "state": verifier,
        });
        let tokens = post_token(&payload, self.token_url).await?;
        self.store(tokens).await
    }

    /// Operator-facing login status (`stoke login-claude --status`): whether
    /// the store holds usable tokens and when the access token expires. A
    /// missing, malformed, or wrongly-permissioned store reads as "not logged
    /// in" — status never fails hard and never surfaces token material.
    pub async fn status(&self) -> LoginStatus {
        if self.ensure_loaded().await.is_err() {
            return LoginStatus {
                logged_in: false,
                expires_at: None,
            };
        }
        let state = self.state.lock().await;
        match state.as_ref() {
            Some(tokens) => LoginStatus {
                logged_in: true,
                expires_at: Some(tokens.expires_at),
            },
            None => LoginStatus {
                logged_in: false,
                expires_at: None,
            },
        }
    }
}

/// `~/.stoke/anthropic_oauth.json`
pub fn default_store_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join(".stoke")
        .join("anthropic_oauth.json")
}

// ---------------------------------------------------------------------------
// RFC 7636 PKCE (S256)
// ---------------------------------------------------------------------------

/// A fresh (verifier, challenge) pair. The verifier is 43 base64url
/// characters — inside RFC 7636's 43..=128 range and built only from its
/// unreserved character set.
pub fn pkce_pair() -> (String, String) {
    let verifier = generate_verifier();
    let challenge = code_challenge(&verifier);
    (verifier, challenge)
}

/// code_challenge = BASE64URL-ENCODE(SHA256(ASCII(code_verifier))), no padding.
pub fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// 32 bytes of OS entropy, base64url-encoded (43 verifier characters).
fn generate_verifier() -> String {
    let mut bytes = [0u8; 32];
    match std::fs::File::open("/dev/urandom") {
        Ok(mut source) => {
            if source.read_exact(&mut bytes).is_err() {
                fill_weak(&mut bytes);
            }
        }
        Err(_) => fill_weak(&mut bytes),
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Fallback for platforms without /dev/urandom: mix wall-clock and pid
/// through SHA-256. Weaker than the OS CSPRNG but never empty.
fn fill_weak(bytes: &mut [u8; 32]) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mixed = format!("{nanos}:{}:{}", std::process::id(), AUTHORIZE_URL);
    *bytes = Sha256::digest(mixed.as_bytes()).into();
}

/// Percent-encode a query component per RFC 3986: unreserved characters pass
/// through, everything else becomes %XX. The official Claude Code client sends
/// `%20` for spaces in the scope, so Stoke builds the query by hand instead of
/// relying on the `url` crate (which would emit `+`).
fn percent_encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The authorize URL the official Claude Code client would open, with `state`
/// bound to the PKCE verifier (that is what the real client sends).
pub fn authorization_url(verifier: &str, challenge: &str, redirect_uri: &str) -> String {
    format!(
        "{AUTHORIZE_URL}?code=true&client_id={client}&response_type=code&redirect_uri={redirect}&scope={scope}&code_challenge={challenge}&code_challenge_method=S256&state={state}",
        client = percent_encode_component(CLIENT_ID),
        redirect = percent_encode_component(redirect_uri),
        scope = percent_encode_component(OAUTH_SCOPES),
        challenge = percent_encode_component(challenge),
        state = percent_encode_component(verifier),
    )
}

// ---------------------------------------------------------------------------
// Local loopback callback
// ---------------------------------------------------------------------------

/// Parse the `GET <target>` request line of the browser's redirect.
pub fn parse_callback(target: &str) -> Result<(String, String), String> {
    let prefixed = if target.starts_with('/') {
        format!("http://localhost{target}")
    } else {
        target.to_string()
    };
    let url = reqwest::Url::parse(&prefixed).map_err(|_| "malformed OAuth callback".to_string())?;
    if url.path() != "/callback" {
        return Err(format!("unexpected OAuth callback path {}", url.path()));
    }
    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    match (code, state) {
        (Some(code), Some(state)) if !code.is_empty() => Ok((code, state)),
        _ => Err("OAuth callback missing code or state (authorization was refused)".to_string()),
    }
}

fn wait_for_callback(listener: std::net::TcpListener) -> Result<(String, String), String> {
    let (mut stream, _) = listener
        .accept()
        .map_err(|e| format!("callback listener failed: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(CALLBACK_TIMEOUT_SECS)))
        .map_err(|e| format!("callback listener setup failed: {e}"))?;
    let target = read_request_target(&mut stream)?;
    let result = parse_callback(&target);
    let body = "<html><body><h1>Stoke</h1><p>Login received — you can close this window.</p></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    result
}

/// Read a GET request far enough to extract the request target. GETs carry no
/// body, so the first line is all we need.
fn read_request_target(stream: &mut std::net::TcpStream) -> Result<String, String> {
    let mut buf = [0u8; 4096];
    let mut received = Vec::new();
    loop {
        let read = stream
            .read(&mut buf)
            .map_err(|e| format!("failed reading OAuth callback request: {e}"))?;
        if read == 0 {
            break;
        }
        received.extend_from_slice(&buf[..read]);
        if received.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if received.len() > 32 * 1024 {
            return Err("OAuth callback request too large".to_string());
        }
    }
    let head = String::from_utf8_lossy(&received);
    let request_line = head.lines().next().unwrap_or_default();
    let target = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "malformed OAuth callback request".to_string())?;
    Ok(target.to_string())
}

/// Open the operator's browser at `url`. Public so the login CLI can reuse it
/// after printing the authorize URL.
pub fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let program = "xdg-open";
    #[cfg(target_os = "windows")]
    let program = "cmd";
    #[cfg(target_os = "windows")]
    let args = ["/C".to_string(), "start".to_string(), url.to_string()];
    #[cfg(not(target_os = "windows"))]
    let args = [url.to_string()];
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map_err(|e| format!("failed to open a browser for the OAuth login: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Token endpoint
// ---------------------------------------------------------------------------

/// POST to the token endpoint. The official client sends JSON (not
/// form-urlencoded) with `state` in the exchange body; the response carries
/// `expires_in` seconds. Error paths report status codes only — token
/// material never enters an error string.
async fn post_token(payload: &Value, token_url: &str) -> Result<TokenSet, String> {
    let response = OAUTH_CLIENT
        .post(token_url)
        .header("content-type", "application/json")
        .json(payload)
        .send()
        .await
        .map_err(|e| format!("token endpoint unreachable: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        // Deliberately no body in the error: bodies can echo request secrets.
        return Err(format!("token endpoint returned HTTP {status}"));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|e| format!("token endpoint returned invalid JSON: {e}"))?;
    tokens_from_response(&body)
}

fn tokens_from_response(body: &Value) -> Result<TokenSet, String> {
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or("token response missing access_token")?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let now = chrono::Utc::now().timestamp();
    let expires_at = if let Some(seconds) = body.get("expires_in").and_then(Value::as_i64) {
        now + seconds.max(0)
    } else if let Some(expires_at) = body.get("expires_at").and_then(Value::as_i64) {
        expires_at
    } else {
        return Err("token response missing expires_in".to_string());
    };
    Ok(TokenSet {
        access_token,
        refresh_token,
        expires_at,
        created_at: now,
    })
}

// ---------------------------------------------------------------------------
// Store on disk — 0600 file in a 0700 directory, atomic rewrites
// ---------------------------------------------------------------------------

fn write_store(path: &PathBuf, tokens: &TokenSet) -> Result<(), String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
        }
        // The directory holding tokens must be operator-only. If it exists
        // with looser perms we tighten it rather than write into a leak.
        let meta = std::fs::metadata(parent)
            .map_err(|e| format!("failed to stat {}: {e}", parent.display()))?;
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("failed to tighten {} to 0700: {e}", parent.display()))?;
        }
    }

    // An existing store that others could read is a compromised credential:
    // refuse to bless it with new tokens until the operator fixes the mode.
    if let Ok(meta) = std::fs::metadata(path) {
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "refusing to overwrite world-readable token store {} (chmod 600 it first)",
                path.display()
            ));
        }
    }

    let tmp = path.with_extension("json.tmp");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("failed to create {}: {e}", tmp.display()))?;
    let serialized = serde_json::to_string_pretty(tokens)
        .map_err(|e| format!("failed to serialize token store: {e}"))?;
    let mut handle = file;
    if let Err(e) = handle
        .write_all(serialized.as_bytes())
        .and_then(|_| handle.sync_all())
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("failed to write {}: {e}", tmp.display()));
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("failed to finalize {}: {e}", path.display()))?;
    // Belt and braces: rename can carry the tmp file's mode, which we set at
    // creation, but re-assert 0600 on the final path in case of surprises.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

/// Read the store.
///
/// - Missing file → None ("not logged in").
/// - Malformed JSON → None, same meaning: treat as not logged in, never panic.
/// - Group/world-readable → refused with an explicit error; those tokens are
///   compromised and must not be used.
fn read_store(path: &PathBuf) -> Option<Result<TokenSet, String>> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).ok()?;
    let mode = meta.permissions().mode();
    if mode & 0o077 != 0 {
        return Some(Err(format!(
            "Anthropic OAuth token store {} is group/world readable — refusing to use it (chmod 600 it first)",
            path.display()
        )));
    }
    let content = std::fs::read_to_string(path).ok()?;
    Some(serde_json::from_str(&content).map_err(|_| {
        format!(
            "Anthropic OAuth token store at {} is malformed; treating it as not logged in",
            path.display()
        )
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_store(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stoke-oauth-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("anthropic_oauth.json")
    }

    fn cleanup(path: &PathBuf) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn pkce_matches_rfc7636_appendix_b() {
        // https://datatracker.ietf.org/doc/html/rfc7636#appendix-B
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            code_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifiers_are_in_rfc_range_and_charset() {
        let (verifier, challenge) = pkce_pair();
        assert!(
            (43..=128).contains(&verifier.len()),
            "verifier length {} outside RFC 7636 range",
            verifier.len()
        );
        assert!(
            verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "verifier must use only unreserved characters"
        );
        assert_eq!(code_challenge(&verifier), challenge);
        let (verifier2, _) = pkce_pair();
        assert_ne!(verifier, verifier2, "verifiers must not repeat");
    }

    #[test]
    fn authorize_url_sends_exactly_the_official_client_parameters() {
        let (verifier, challenge) = pkce_pair();
        let url = authorization_url(&verifier, &challenge, "http://localhost:51777/callback");
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains(&format!("client_id={CLIENT_ID}")));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A51777%2Fcallback"));
        assert!(url.contains(
            "scope=user%3Aprofile%20user%3Ainference%20user%3Asessions%3Aclaude_code%20user%3Amcp_servers%20user%3Afile_upload"
        ));
        assert_eq!(
            OAUTH_SCOPES,
            "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"
        );
        assert!(url.contains(&format!("code_challenge={challenge}")));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("state={verifier}")));
    }

    #[tokio::test]
    async fn store_writes_0600_file_inside_0700_directory() {
        let path = temp_store("perms");
        let store = TokenStore::with_path(path.clone());
        let tokens = TokenSet {
            access_token: "at-test".into(),
            refresh_token: "rt-test".into(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            created_at: chrono::Utc::now().timestamp(),
        };
        store.store(tokens).await.expect("store must succeed");

        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(file_mode & 0o777, 0o600, "token file must be 0600");
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700, "token directory must be 0700");

        let round: TokenSet =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(round.access_token, "at-test");
        assert_eq!(round.refresh_token, "rt-test");

        cleanup(&path);
    }

    #[tokio::test]
    async fn store_refuses_a_world_readable_file() {
        let path = temp_store("world-readable");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let store = TokenStore::with_path(path.clone());
        // Loading a compromised store must fail, not use it.
        assert!(store.get_valid_access_token().await.is_err());
        // And so must writing fresh tokens over it.
        let tokens = TokenSet {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            expires_at: chrono::Utc::now().timestamp() + 60,
            created_at: chrono::Utc::now().timestamp(),
        };
        let err = store.store(tokens).await.unwrap_err();
        assert!(err.contains("world-readable"), "error must explain: {err}");

        cleanup(&path);
    }

    #[tokio::test]
    async fn malformed_store_json_is_treated_as_not_logged_in_without_panicking() {
        let path = temp_store("malformed");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ this is not json !!!").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let store = TokenStore::with_path(path.clone());
        let err = store.get_valid_access_token().await.unwrap_err();
        assert!(err.contains("not logged in"), "error must explain: {err}");
        // A later login must not be poisoned by the earlier garbage read.
        let tokens = TokenSet {
            access_token: "at-fresh".into(),
            refresh_token: "rt-fresh".into(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            created_at: chrono::Utc::now().timestamp(),
        };
        store.store(tokens).await.expect("login must recover");
        let token = store.get_valid_access_token().await.unwrap();
        assert_eq!(token, "at-fresh");

        cleanup(&path);
    }

    /// One-shot std TcpListener token-endpoint mock (no axum in unit tests).
    /// Serves exactly one request, hands the raw request to `check`, and
    /// answers with `body`.
    fn spawn_token_mock<F>(check: F, response_body: &'static str) -> &'static str
    where
        F: FnOnce(&str) + Send + 'static,
    {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..pos]);
                    let content_length: usize = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if raw.len() >= pos + 4 + content_length {
                        break;
                    }
                }
            }
            let request = String::from_utf8_lossy(&raw).to_string();
            check(&request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        Box::leak(format!("http://{addr}/v1/oauth/token").into_boxed_str())
    }

    fn seed(path: &PathBuf, access: &str, refresh: &str, expires_in: i64) {
        let tokens = TokenSet {
            access_token: access.into(),
            refresh_token: refresh.into(),
            expires_at: chrono::Utc::now().timestamp() + expires_in,
            created_at: chrono::Utc::now().timestamp(),
        };
        write_store(path, &tokens).expect("seed store");
    }

    #[tokio::test]
    async fn refresh_sends_the_official_refresh_grant_body() {
        let path = temp_store("refresh-body");
        let store = TokenStore::with_token_url(
            path.clone(),
            spawn_token_mock(
                |request| {
                    assert!(request.starts_with("POST /v1/oauth/token HTTP/1.1"));
                    assert!(request
                        .to_ascii_lowercase()
                        .contains("content-type: application/json"));
                    assert!(request.contains(r#""grant_type":"refresh_token""#));
                    assert!(request.contains(r#""refresh_token":"rt-old""#));
                    assert!(request.contains(&format!(r#""client_id":"{CLIENT_ID}""#)));
                },
                r#"{"access_token":"at-new","refresh_token":"rt-new","expires_in":3600}"#,
            ),
        );

        seed(&path, "at-old", "rt-old", 1); // near expiry

        let token = store
            .get_valid_access_token()
            .await
            .expect("refresh must succeed");
        assert_eq!(token, "at-new");

        // The rotated tokens are on disk and in memory.
        let stored: TokenSet =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(stored.refresh_token, "rt-new");
        assert!(stored.expires_at > chrono::Utc::now().timestamp() + EXPIRY_SKEW_SECS);

        cleanup(&path);
    }

    #[tokio::test]
    async fn refresh_failure_leaves_the_previous_tokens_in_place() {
        // An unreachable token endpoint must produce Err, keep the old store,
        // and never panic.
        let path = temp_store("refresh-failure");
        let store = TokenStore::with_token_url(path.clone(), "http://127.0.0.1:1/v1/oauth/token");
        seed(&path, "at-old", "rt-old", 1);

        let err = store.get_valid_access_token().await.unwrap_err();
        assert!(err.contains("refresh failed"), "error must explain: {err}");

        let stored: TokenSet =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(stored.refresh_token, "rt-old", "old tokens must survive");
        assert_eq!(stored.access_token, "at-old");

        cleanup(&path);
    }

    #[tokio::test]
    async fn refreshed_store_without_a_new_refresh_token_keeps_the_old_one() {
        let path = temp_store("refresh-keep");
        let store = TokenStore::with_token_url(
            path.clone(),
            spawn_token_mock(|_| {}, r#"{"access_token":"at-new","expires_in":7200}"#),
        );
        seed(&path, "at-old", "rt-old", 1);

        let token = store.get_valid_access_token().await.unwrap();
        assert_eq!(token, "at-new");
        let stored: TokenSet =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(stored.refresh_token, "rt-old");

        cleanup(&path);
    }

    #[tokio::test]
    async fn cached_token_is_returned_until_the_skew_window() {
        let path = temp_store("skew");
        // token_url points at a port with no listener: any network attempt
        // fails loudly, so a successful return proves the cache was used.
        let url = "http://127.0.0.1:1/v1/oauth/token";
        let store = TokenStore::with_token_url(path.clone(), url);

        seed(&path, "at-cached", "rt", 3600);
        assert_eq!(
            store.get_valid_access_token().await.unwrap(),
            "at-cached",
            "a token with more than 60s of life must be served from cache"
        );

        // A token inside the 60s skew window must trigger refresh instead.
        seed(&path, "at-stale", "rt", 30);
        let store2 = TokenStore::with_token_url(path.clone(), url);
        assert!(
            store2.get_valid_access_token().await.is_err(),
            "a token inside the skew window must trigger refresh, and the endpoint is down"
        );

        cleanup(&path);
    }

    #[tokio::test]
    async fn concurrent_callers_refresh_once_not_once_each() {
        let path = temp_store("concurrent");
        let store = std::sync::Arc::new(TokenStore::with_token_url(
            path.clone(),
            spawn_token_mock(
                |_| {},
                r#"{"access_token":"at-refreshed","refresh_token":"rt-new","expires_in":3600}"#,
            ),
        ));

        seed(&path, "at-old", "rt-old", 1); // near expiry

        let (a, b) = tokio::join!(
            store.get_valid_access_token(),
            store.get_valid_access_token()
        );
        // The mock serves exactly one request, so both succeeding proves the
        // second caller reused the refreshed token instead of re-freshening.
        assert_eq!(a.expect("first caller"), "at-refreshed");
        assert_eq!(b.expect("second caller"), "at-refreshed");

        cleanup(&path);
    }

    #[test]
    fn access_tokens_never_reach_debug_output_or_error_strings() {
        let tokens = TokenSet {
            access_token: "sk-ant-super-secret".into(),
            refresh_token: "rt-super-secret".into(),
            expires_at: 123,
            created_at: 456,
        };
        let printed = format!("{tokens:?}");
        assert!(!printed.contains("sk-ant-super-secret"));
        assert!(!printed.contains("rt-super-secret"));
        assert!(printed.contains("[REDACTED]"));

        // Error paths must not carry token material either.
        let err = tokens_from_response(&json!({})).unwrap_err();
        assert!(!err.contains("sk-ant"));
        assert_eq!(
            err, "token response missing access_token",
            "errors describe the problem, not the secret"
        );
    }

    #[test]
    fn parse_callback_extracts_code_and_state() {
        let (code, state) =
            parse_callback("/callback?code=authcode&state=verifier-value_1").unwrap();
        assert_eq!(code, "authcode");
        assert_eq!(state, "verifier-value_1");

        // Encoded query values decode correctly.
        let (code, state) = parse_callback("/callback?code=a%2Fb&state=ver%20ifier").unwrap();
        assert_eq!(code, "a/b");
        assert_eq!(state, "ver ifier");

        assert!(parse_callback("/callback?state=only").is_err());
        assert!(parse_callback("/callback").is_err());
        assert!(parse_callback("/other?code=a&state=b").is_err());
        assert!(parse_callback("garbage not a url").is_err());
    }

    #[tokio::test]
    async fn missing_refresh_token_is_an_error_not_a_panic() {
        let path = temp_store("no-refresh");
        let store = TokenStore::with_token_url(path.clone(), "http://127.0.0.1:1/v1/oauth/token");
        seed(&path, "at", "", 1);
        let err = store.refresh().await.unwrap_err();
        assert!(
            err.contains("no refresh token"),
            "error must explain: {err}"
        );
        cleanup(&path);
    }
}
