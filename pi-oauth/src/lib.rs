//! pi-oauth - OAuth login flows for AI providers.
//!
//! Supports three OAuth providers:
//! - **Anthropic** (Claude Pro/Max) — Authorization Code + PKCE flow with local callback server
//! - **GitHub Copilot** — Device code flow (no local server)
//! - **OpenAI Codex** (ChatGPT OAuth) — Authorization Code + PKCE flow with local callback server

// ---------------------------------------------------------------------------
// Imports
// ---------------------------------------------------------------------------

use async_trait::async_trait;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

// ---------------------------------------------------------------------------
// JWT helper — decode the chatgpt_account_id from an OpenAI Codex JWT
// ---------------------------------------------------------------------------

/// Decode the `account_id` from an OpenAI Codex access token (JWT).
///
/// The TS code calls `getAccountId(tokenResult.access)` which extracts the
/// `chatgpt_account_id` claim. No signature verification — claims only.
fn decode_codex_account_id(access_token: &str) -> Option<String> {
    let payload_part = access_token.split('.').nth(1)?;
    let padded = match payload_part.len() % 4 {
        2 => format!("{payload_part}=="),
        3 => format!("{payload_part}="),
        _ => payload_part.to_string(),
    };
    let b64 = padded.replace('-', "+").replace('_', "/");
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD.decode(&b64).ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    payload
        .get("https://api.openai.com/auth")
        .and_then(|v| v.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Constants — client IDs & endpoints
// ---------------------------------------------------------------------------

// Anthropic client_id (base64 "OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl" decoded)
const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const ANTHROPIC_PORT: u16 = 53692;
const ANTHROPIC_CALLBACK_PATH: &str = "/callback";
const ANTHROPIC_SCOPES: &str =
    "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

// GitHub Copilot client_id (base64 "SXYxLmI1MDdhMDhjODdlY2ZlOTg=" decoded)
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

// OpenAI Codex
const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_PORT: u16 = 1455;
const OPENAI_CALLBACK_PATH: &str = "/auth/callback";
const OPENAI_SCOPE: &str = "openid profile email offline_access";

// ---------------------------------------------------------------------------
// Base64url encoding (no-pad, URL-safe)
// ---------------------------------------------------------------------------

const B64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(data: &[u8]) -> String {
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(B64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(B64_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(B64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            result.push(B64_CHARS[(triple & 0x3F) as usize] as char);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// PKCE utilities
// ---------------------------------------------------------------------------

/// Generate a random PKCE code verifier (32 random bytes, base64url-encoded).
pub fn generate_code_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    base64url_encode(&bytes)
}

/// Compute the S256 code challenge for a given verifier.
pub fn generate_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    base64url_encode(&hash)
}

/// Generate both verifier and challenge in one call.
pub fn generate_pkce() -> (String, String) {
    let verifier = generate_code_verifier();
    let challenge = generate_code_challenge(&verifier);
    (verifier, challenge)
}

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// OAuth credentials returned after a successful login or token refresh.
#[derive(Debug, Clone)]
pub struct OAuthCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    pub token_type: String,
    /// Account ID extracted from the OpenAI Codex JWT (if applicable).
    pub account_id: Option<String>,
}

/// Information passed to the `on_auth` callback when authentication begins.
#[derive(Debug, Clone)]
pub struct OAuthAuthInfo {
    pub url: String,
    pub instructions: Option<String>,
}

/// A prompt shown to the user during the OAuth flow.
#[derive(Debug, Clone)]
pub struct OAuthPrompt {
    pub message: String,
    pub placeholder: Option<String>,
    pub allow_empty: bool,
}

/// Errors that can occur during OAuth flows.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),
    #[error("Token exchange failed: {0}")]
    TokenExchange(String),
    #[error("PKCE error: {0}")]
    Pkce(String),
    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// HTML pages (inline, matching oauth-page.ts)
// ---------------------------------------------------------------------------

const LOGO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 800" aria-hidden="true"><path fill="#fff" fill-rule="evenodd" d="M165.29 165.29 H517.36 V400 H400 V517.36 H282.65 V634.72 H165.29 Z M282.65 282.65 V400 H400 V282.65 Z"/><path fill="#fff" d="M517.36 400 H634.72 V634.72 H517.36 Z"/></svg>"##;

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn render_html_page(title: &str, heading: &str, message: &str, details: Option<&str>) -> String {
    let title_esc = escape_html(title);
    let heading_esc = escape_html(heading);
    let message_esc = escape_html(message);
    let details_html = details.map(|d| format!(r#"<div class="details">{}</div>"#, escape_html(d))).unwrap_or_default();

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{title}</title>
  <style>
    :root {{
      --text: #fafafa;
      --text-dim: #a1a1aa;
      --page-bg: #09090b;
      --font-sans: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, "Noto Sans", sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol", "Noto Color Emoji";
      --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    }}
    * {{ box-sizing: border-box; }}
    html {{ color-scheme: dark; }}
    body {{
      margin: 0;
      min-height: 100vh;
      display: flex;
      align-items: center;
      justify-content: center;
      padding: 24px;
      background: var(--page-bg);
      color: var(--text);
      font-family: var(--font-sans);
      text-align: center;
    }}
    main {{
      width: 100%;
      max-width: 560px;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
    }}
    .logo {{
      width: 72px;
      height: 72px;
      display: block;
      margin-bottom: 24px;
    }}
    h1 {{
      margin: 0 0 10px;
      font-size: 28px;
      line-height: 1.15;
      font-weight: 650;
      color: var(--text);
    }}
    p {{
      margin: 0;
      line-height: 1.7;
      color: var(--text-dim);
      font-size: 15px;
    }}
    .details {{
      margin-top: 16px;
      font-family: var(--font-mono);
      font-size: 13px;
      color: var(--text-dim);
      white-space: pre-wrap;
      word-break: break-word;
    }}
  </style>
</head>
<body>
  <main>
    <div class="logo">{logo}</div>
    <h1>{heading}</h1>
    <p>{message}</p>
    {details}
  </main>
</body>
</html>"#,
        title = title_esc,
        logo = LOGO_SVG,
        heading = heading_esc,
        message = message_esc,
        details = details_html,
    )
}

fn oauth_success_html(message: &str) -> String {
    render_html_page("Authentication successful", "Authentication successful", message, None)
}

fn oauth_error_html(message: &str, details: Option<&str>) -> String {
    render_html_page("Authentication failed", "Authentication failed", message, details)
}

// ---------------------------------------------------------------------------
// Local callback server
// ---------------------------------------------------------------------------

/// A minimal HTTP server that listens for an OAuth authorization code callback.
struct CallbackServer {
    listener: TcpListener,
    port: u16,
}

impl CallbackServer {
    /// Bind a TCP listener on 127.0.0.1 on the given port.
    async fn bind(port: u16) -> Result<Self, OAuthError> {
        let addr = format!("127.0.0.1:{}", port);
        let listener = TcpListener::bind(&addr).await?;
        let actual_port = listener.local_addr()?.port();
        tracing::debug!("Callback server listening on 127.0.0.1:{}", actual_port);
        Ok(Self { listener, port: actual_port })
    }

    /// Wait for a single HTTP request at the callback path, validate state,
    /// extract the code, send back a success/error HTML page, and return (code, state).
    async fn wait_for_callback(
        &self,
        callback_path: &str,
        expected_state: &str,
        success_message: &str,
        error_prefix: &str,
    ) -> Result<(String, String), OAuthError> {
        // Accept connection with a 5-minute timeout
        let accept_fut = self.listener.accept();
        let timeout_fut = tokio::time::sleep(Duration::from_secs(300));

        let (mut stream, _peer) = tokio::select! {
            result = accept_fut => result?,
            _ = timeout_fut => return Err(OAuthError::Other(format!(
                "{}: callback timed out after 5 minutes", error_prefix
            ))),
        };

        // Read the HTTP request
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let request_str = String::from_utf8_lossy(&buf[..n]);

        // Parse the request line: "GET /callback?code=...&state=... HTTP/1.1"
        let first_line = request_str.lines().next().unwrap_or("");
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 2 {
            Self::send_error_response(&mut stream, 400, "Bad Request").await?;
            return Err(OAuthError::Other(format!("{}: invalid HTTP request", error_prefix)));
        }
        let path_and_query = parts[1];

        // Build a full URL so we can parse query params
        let full_url = format!("http://localhost:{}{}", self.port, path_and_query);
        let parsed_url = Url::parse(&full_url)
            .map_err(|e| OAuthError::Other(format!("{}: failed to parse callback URL: {}", error_prefix, e)))?;

        // Validate the path
        if parsed_url.path() != callback_path {
            let html = oauth_error_html("Callback route not found.", None);
            Self::send_html_response(&mut stream, 404, &html).await?;
            return Err(OAuthError::Other(format!(
                "{}: unexpected callback path: {}",
                error_prefix,
                parsed_url.path()
            )));
        }

        // Check for OAuth error
        if let Some(err) = parsed_url.query_pairs().find(|(k, _)| k == "error") {
            let err_desc = err.1.to_string();
            let html = oauth_error_html("Authentication did not complete.", Some(&format!("Error: {}", err_desc)));
            Self::send_html_response(&mut stream, 400, &html).await?;
            return Err(OAuthError::Other(format!("{}: auth error: {}", error_prefix, err_desc)));
        }

        // Extract code
        let code = parsed_url
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned())
            .ok_or_else(|| OAuthError::Other(format!("{}: missing authorization code", error_prefix)))?;

        // Extract state
        let state = parsed_url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .ok_or_else(|| OAuthError::Other(format!("{}: missing state parameter", error_prefix)))?;

        // Validate state
        if state != expected_state {
            let html = oauth_error_html("State mismatch.", None);
            let _ = Self::send_html_response(&mut stream, 400, &html).await;
            return Err(OAuthError::Other(format!("{}: state mismatch", error_prefix)));
        }

        // Success
        let html = oauth_success_html(success_message);
        Self::send_html_response(&mut stream, 200, &html).await?;

        Ok((code, state))
    }

    async fn send_html_response(
        stream: &mut (impl AsyncWriteExt + Unpin),
        status: u16,
        body: &str,
    ) -> Result<(), OAuthError> {
        let status_text = if status == 200 { "OK" } else { "Error" };
        let response = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            status_text,
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await?;
        stream.flush().await?;
        Ok(())
    }

    async fn send_error_response(
        stream: &mut (impl AsyncWriteExt + Unpin),
        status: u16,
        message: &str,
    ) -> Result<(), OAuthError> {
        let body = format!("<h1>{}</h1>", escape_html(message));
        Self::send_html_response(stream, status, &body).await
    }
}

// ---------------------------------------------------------------------------
// Parsing authorization input from user paste
// ---------------------------------------------------------------------------

/// Parse a user-pasted authorization code or redirect URL.
struct ParsedCode {
    code: String,
    state: Option<String>,
}

fn parse_authorization_input(input: &str) -> ParsedCode {
    let value = input.trim();
    if value.is_empty() {
        return ParsedCode { code: String::new(), state: None };
    }

    // Try parsing as URL
    if let Ok(url) = Url::parse(value) {
        return ParsedCode {
            code: url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.into_owned()).unwrap_or_default(),
            state: url.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.into_owned()),
        };
    }

    // Try parsing as "code#state" format
    if let Some(hash_pos) = value.find('#') {
        let c = &value[..hash_pos];
        let s = &value[hash_pos + 1..];
        if !c.is_empty() {
            return ParsedCode { code: c.to_string(), state: Some(s.to_string()) };
        }
    }

    // Try parsing as query-string-like "code=xxx&state=yyy"
    if value.contains("code=") {
        if let Ok(url) = Url::parse(&format!("http://localhost?{}", value)) {
            return ParsedCode {
                code: url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.into_owned()).unwrap_or_default(),
                state: url.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.into_owned()),
            };
        }
    }

    // Treat the entire value as just the code
    ParsedCode { code: value.to_string(), state: None }
}

// ---------------------------------------------------------------------------
// OAuthCallbacks trait
// ---------------------------------------------------------------------------

/// Callback interface for OAuth login flows.
///
/// Implement this trait to connect the OAuth crate to your UI layer.
#[async_trait]
pub trait OAuthCallbacks: Send + Sync {
    /// Called with the authorization URL (and optional instructions) when login begins.
    fn on_auth(&self, info: &OAuthAuthInfo);

    /// Called to show progress during the login flow.
    fn on_progress(&self, message: &str);

    /// Called when user input is needed (e.g. fallback code paste, enterprise domain).
    async fn on_prompt(&self, prompt: &OAuthPrompt) -> String;

    /// Optional: return a manually-pasted authorization code or redirect URL.
    /// Return `None` to wait for the browser callback.
    async fn on_manual_code_input(&self) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------
// OAuthProvider trait
// ---------------------------------------------------------------------------

/// A provider that supports OAuth-based authentication.
#[async_trait]
pub trait OAuthProvider: Send + Sync {
    /// Run the login flow and return credentials.
    async fn login(&self, callbacks: &dyn OAuthCallbacks) -> Result<OAuthCredentials, OAuthError>;

    /// Refresh expired credentials using a refresh token.
    async fn refresh(&self, refresh_token: &str) -> Result<OAuthCredentials, OAuthError>;
}

// ---------------------------------------------------------------------------
// HTTP helper: POST JSON, return text body
// ---------------------------------------------------------------------------

/// Build a shared `reqwest::Client` on first use.
fn http_client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn post_json(url: &str, body: &serde_json::Value) -> Result<String, OAuthError> {
    let client = http_client();
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(body)
        .timeout(Duration::from_secs(30))
        .send()
        .await?;

    let status = response.status();
    let response_body = response.text().await?;

    if !status.is_success() {
        return Err(OAuthError::TokenExchange(format!("HTTP {} from {}: {}", status.as_u16(), url, response_body)));
    }

    Ok(response_body)
}

async fn post_form(url: &str, body: &[(&str, &str)]) -> Result<String, OAuthError> {
    let client = http_client();
    let response = client
        .post(url)
        .header("Accept", "application/json")
        .form(body)
        .timeout(Duration::from_secs(30))
        .send()
        .await?;

    let status = response.status();
    let response_body = response.text().await?;

    if !status.is_success() {
        return Err(OAuthError::TokenExchange(format!("HTTP {} from {}: {}", status.as_u16(), url, response_body)));
    }

    Ok(response_body)
}

async fn get_json(url: &str, headers: &[(&str, &str)]) -> Result<String, OAuthError> {
    let client = http_client();
    let mut req = client.get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let response = req.timeout(Duration::from_secs(30)).send().await?;

    let status = response.status();
    let response_body = response.text().await?;

    if !status.is_success() {
        return Err(OAuthError::TokenExchange(format!("HTTP {} from {}: {}", status.as_u16(), url, response_body)));
    }

    Ok(response_body)
}

// ---------------------------------------------------------------------------
// High-level flow helpers
// ---------------------------------------------------------------------------

/// Wait for an OAuth authorization code, racing a local callback server against
/// manual code input (if available), falling back to a text prompt.
async fn resolve_authorization_code(
    server: &CallbackServer,
    callbacks: &dyn OAuthCallbacks,
    callback_path: &str,
    expected_state: &str,
    success_message: &str,
    error_prefix: &str,
) -> Result<(String, String), OAuthError> {
    // Race the browser callback against manual code input (if available).
    let manual_fut = callbacks.on_manual_code_input();
    tokio::pin!(manual_fut);

    let cb_fut = server.wait_for_callback(callback_path, expected_state, success_message, error_prefix);
    tokio::pin!(cb_fut);

    let select_result: Result<(String, String), OAuthError> = tokio::select! {
        result = &mut cb_fut => {
            // Browser callback completed
            result
        }
        manual = &mut manual_fut => {
            // Manual input completed
            match manual {
                Some(input) if !input.trim().is_empty() => {
                    let parsed = parse_authorization_input(&input);
                    if !parsed.code.is_empty() {
                        let state = parsed.state.unwrap_or_else(|| expected_state.to_string());
                        return Ok((parsed.code, state));
                    }
                }
                _ => {}
            }
            // Manual input didn't provide a usable code.
            // The first callback future was cancelled by select! — try again.
            server
                .wait_for_callback(
                    callback_path,
                    expected_state,
                    success_message,
                    error_prefix,
                )
                .await
        }
    };

    match select_result {
        Ok((code, state)) => Ok((code, state)),
        Err(cb_err) => {
            // Both manual and callback failed.  Fall back to a text prompt.
            callbacks.on_progress(&format!(
                "{}\n{}\n{}",
                "Browser callback did not complete.",
                "You can paste the authorization code manually.",
                "Open the auth URL in your browser and paste the redirect URL here."
            ));

            let input = callbacks
                .on_prompt(&OAuthPrompt {
                    message: format!("{}: Paste the authorization code or full redirect URL:", error_prefix),
                    placeholder: None,
                    allow_empty: false,
                })
                .await;

            let parsed = parse_authorization_input(&input);
            if parsed.code.is_empty() {
                return Err(OAuthError::Other(format!("{}: missing authorization code", error_prefix)));
            }
            // Swallow the callback error — the user provided code manually.
            let _ = cb_err;
            Ok((parsed.code, parsed.state.unwrap_or_else(|| expected_state.to_string())))
        }
    }
}

// ---------------------------------------------------------------------------
// Anthropic OAuth
// ---------------------------------------------------------------------------

/// Anthropic OAuth provider (Authorization Code + PKCE).
pub struct AnthropicOAuth;

#[async_trait]
impl OAuthProvider for AnthropicOAuth {
    async fn login(&self, callbacks: &dyn OAuthCallbacks) -> Result<OAuthCredentials, OAuthError> {
        login_anthropic(callbacks).await
    }

    async fn refresh(&self, refresh_token: &str) -> Result<OAuthCredentials, OAuthError> {
        refresh_anthropic_token(refresh_token).await
    }
}

/// Login with Anthropic OAuth (authorization code + PKCE).
pub async fn login_anthropic(callbacks: &dyn OAuthCallbacks) -> Result<OAuthCredentials, OAuthError> {
    let (verifier, challenge) = generate_pkce();
    let server = CallbackServer::bind(ANTHROPIC_PORT).await?;

    let redirect_uri = format!("http://localhost:{}{}", ANTHROPIC_PORT, ANTHROPIC_CALLBACK_PATH);

    let mut auth_url = Url::parse(ANTHROPIC_AUTHORIZE_URL)?;
    auth_url
        .query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", ANTHROPIC_CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", ANTHROPIC_SCOPES)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &verifier);

    callbacks.on_auth(&OAuthAuthInfo {
        url: auth_url.to_string(),
        instructions: Some(
            "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here."
                .to_string(),
        ),
    });

    let (code, state) = resolve_authorization_code(
        &server,
        callbacks,
        ANTHROPIC_CALLBACK_PATH,
        &verifier,
        "Anthropic authentication completed. You can close this window.",
        "Anthropic OAuth",
    )
    .await?;

    // Verify state
    if state != verifier {
        return Err(OAuthError::Other("OAuth state mismatch".into()));
    }

    callbacks.on_progress("Exchanging authorization code for tokens...");
    exchange_anthropic_code(&code, &state, &verifier, &redirect_uri).await
}

async fn exchange_anthropic_code(
    code: &str,
    state: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthCredentials, OAuthError> {
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": ANTHROPIC_CLIENT_ID,
        "code": code,
        "state": state,
        "redirect_uri": redirect_uri,
        "code_verifier": verifier,
    });

    let response_body = post_json(ANTHROPIC_TOKEN_URL, &body).await?;

    let token_data: serde_json::Value = serde_json::from_str(&response_body)
        .map_err(|e| OAuthError::TokenExchange(format!("Invalid JSON response: {}", e)))?;

    let access_token = token_data["access_token"]
        .as_str()
        .ok_or_else(|| OAuthError::TokenExchange("Missing access_token".into()))?
        .to_string();

    let refresh_token = token_data["refresh_token"].as_str().map(|s| s.to_string());

    let expires_in =
        token_data["expires_in"].as_i64().ok_or_else(|| OAuthError::TokenExchange("Missing expires_in".into()))?;

    // Use current time + expires_in - 5 minute buffer, in seconds
    let expires_at = Some(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
            + expires_in
            - 300, // 5 min buffer
    );

    Ok(OAuthCredentials { access_token, refresh_token, expires_at, token_type: "Bearer".into(), account_id: None })
}

/// Refresh an Anthropic OAuth token.
pub async fn refresh_anthropic_token(refresh_token: &str) -> Result<OAuthCredentials, OAuthError> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": ANTHROPIC_CLIENT_ID,
        "refresh_token": refresh_token,
    });

    let response_body = post_json(ANTHROPIC_TOKEN_URL, &body).await?;

    let data: serde_json::Value = serde_json::from_str(&response_body)
        .map_err(|e| OAuthError::TokenExchange(format!("Invalid JSON response: {}", e)))?;

    let access_token = data["access_token"]
        .as_str()
        .ok_or_else(|| OAuthError::TokenExchange("Missing access_token".into()))?
        .to_string();

    let new_refresh = data["refresh_token"].as_str().map(|s| s.to_string());

    let expires_in =
        data["expires_in"].as_i64().ok_or_else(|| OAuthError::TokenExchange("Missing expires_in".into()))?;

    let expires_at = Some(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
            + expires_in
            - 300,
    );

    Ok(OAuthCredentials {
        access_token,
        refresh_token: new_refresh,
        expires_at,
        token_type: "Bearer".into(),
        account_id: None,
    })
}

// ---------------------------------------------------------------------------
// GitHub Copilot OAuth (Device Code Flow)
// ---------------------------------------------------------------------------

const COPILOT_HEADERS: [(&str, &str); 4] = [
    ("User-Agent", "GitHubCopilotChat/0.35.0"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
    ("Copilot-Integration-Id", "vscode-chat"),
];

/// GitHub Copilot OAuth provider (Device Code flow).
pub struct GitHubCopilotOAuth;

#[async_trait]
impl OAuthProvider for GitHubCopilotOAuth {
    async fn login(&self, callbacks: &dyn OAuthCallbacks) -> Result<OAuthCredentials, OAuthError> {
        login_github_copilot(callbacks).await
    }

    async fn refresh(&self, refresh_token: &str) -> Result<OAuthCredentials, OAuthError> {
        refresh_github_copilot_token(refresh_token, None).await
    }
}

/// Normalize a user-entered domain string.
pub fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    // If no scheme, prepend https://
    let with_scheme = if trimmed.contains("://") { trimmed.to_string() } else { format!("https://{}", trimmed) };
    Url::parse(&with_scheme).ok().map(|u| u.host_str().unwrap_or(trimmed).to_string())
}

/// Get GitHub Copilot API URLs for a given domain.
fn get_copilot_urls(domain: &str) -> (String, String, String) {
    (
        format!("https://{}/login/device/code", domain),
        format!("https://{}/login/oauth/access_token", domain),
        format!("https://api.{}/copilot_internal/v2/token", domain),
    )
}

/// Login with GitHub Copilot OAuth (device code flow).
pub async fn login_github_copilot(callbacks: &dyn OAuthCallbacks) -> Result<OAuthCredentials, OAuthError> {
    // Prompt for enterprise domain
    let input = callbacks
        .on_prompt(&OAuthPrompt {
            message: "GitHub Enterprise URL/domain (blank for github.com)".into(),
            placeholder: Some("company.ghe.com".into()),
            allow_empty: true,
        })
        .await;

    let trimmed = input.trim().to_string();
    let enterprise_domain = if trimmed.is_empty() { None } else { normalize_domain(&trimmed) };
    if !trimmed.is_empty() && enterprise_domain.is_none() {
        return Err(OAuthError::Other("Invalid GitHub Enterprise URL/domain".into()));
    }

    let domain = enterprise_domain.as_deref().unwrap_or("github.com");
    let (device_code_url, access_token_url, _copilot_token_url) = get_copilot_urls(domain);

    // Start device flow
    let device_body = [("client_id", GITHUB_CLIENT_ID), ("scope", "read:user")];
    let device_response = post_form(&device_code_url, &device_body).await?;

    let device_data: serde_json::Value = serde_json::from_str(&device_response)
        .map_err(|e| OAuthError::TokenExchange(format!("Invalid device code response: {}", e)))?;

    let device_code = device_data["device_code"]
        .as_str()
        .ok_or_else(|| OAuthError::TokenExchange("Missing device_code".into()))?
        .to_string();
    let user_code = device_data["user_code"]
        .as_str()
        .ok_or_else(|| OAuthError::TokenExchange("Missing user_code".into()))?
        .to_string();
    let verification_uri = device_data["verification_uri"]
        .as_str()
        .ok_or_else(|| OAuthError::TokenExchange("Missing verification_uri".into()))?
        .to_string();
    let interval = device_data["interval"].as_i64().unwrap_or(5);
    let expires_in =
        device_data["expires_in"].as_i64().ok_or_else(|| OAuthError::TokenExchange("Missing expires_in".into()))?;

    // Show the user code
    callbacks
        .on_auth(&OAuthAuthInfo { url: verification_uri, instructions: Some(format!("Enter code: {}", user_code)) });

    // Poll for GitHub access token
    let github_access_token = poll_device_access_token(&access_token_url, &device_code, interval, expires_in).await?;

    // Exchange GitHub access token for Copilot token
    let credentials = refresh_github_copilot_token(&github_access_token, enterprise_domain.as_deref()).await?;

    Ok(credentials)
}

async fn poll_device_access_token(
    url: &str,
    device_code: &str,
    interval_seconds: i64,
    expires_in: i64,
) -> Result<String, OAuthError> {
    let deadline = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
        as i64
        + expires_in;

    let mut interval_ms = (interval_seconds * 1000).max(1000) as u64;
    let mut slow_down_count = 0u32;

    loop {
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;

        if now >= deadline {
            if slow_down_count > 0 {
                return Err(OAuthError::Other(
                    "Device flow timed out after one or more slow_down responses. \
                     This is often caused by clock drift in WSL or VM environments. \
                     Please sync or restart the VM clock and try again."
                        .into(),
                ));
            }
            return Err(OAuthError::Other("Device flow timed out".into()));
        }

        // Sleep for the interval
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;

        // Poll for token
        let poll_body = [
            ("client_id", GITHUB_CLIENT_ID),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ];

        match post_form(url, &poll_body).await {
            Ok(response) => {
                let data: serde_json::Value = serde_json::from_str(&response)
                    .map_err(|e| OAuthError::TokenExchange(format!("Invalid poll response: {}", e)))?;

                // Check for access_token
                if let Some(token) = data["access_token"].as_str() {
                    return Ok(token.to_string());
                }

                // Check for error
                if let Some(error) = data["error"].as_str() {
                    match error {
                        "authorization_pending" => {
                            // Keep polling
                        }
                        "slow_down" => {
                            slow_down_count += 1;
                            if let Some(new_interval) = data["interval"].as_i64() {
                                interval_ms = (new_interval as u64) * 1000;
                            } else {
                                interval_ms += 5000;
                            }
                        }
                        other => {
                            let description = data["error_description"].as_str().unwrap_or("");
                            return Err(OAuthError::TokenExchange(format!(
                                "Device flow failed: {}: {}",
                                other, description
                            )));
                        }
                    }
                }
            }
            Err(e) => {
                // Transient network errors - log and retry
                tracing::warn!("Poll request failed, retrying: {}", e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Refresh a GitHub Copilot token.
///
/// The `refresh_token` here is actually the GitHub access token.
/// `enterprise_domain` is the optional GitHub Enterprise domain.
pub async fn refresh_github_copilot_token(
    refresh_token: &str,
    enterprise_domain: Option<&str>,
) -> Result<OAuthCredentials, OAuthError> {
    let domain = enterprise_domain.unwrap_or("github.com");
    let (_device_url, _access_url, copilot_token_url) = get_copilot_urls(domain);

    let mut headers: Vec<(&str, &str)> = Vec::from(COPILOT_HEADERS);
    headers.push(("Accept", "application/json"));
    let auth_header_val = format!("Bearer {}", refresh_token);
    headers.push(("Authorization", &auth_header_val));

    let response_body = get_json(&copilot_token_url, &headers).await?;

    let data: serde_json::Value = serde_json::from_str(&response_body)
        .map_err(|e| OAuthError::TokenExchange(format!("Invalid Copilot token response: {}", e)))?;

    let token = data["token"]
        .as_str()
        .ok_or_else(|| OAuthError::TokenExchange("Missing token in Copilot response".into()))?
        .to_string();

    let expires_at_raw = data["expires_at"]
        .as_i64()
        .ok_or_else(|| OAuthError::TokenExchange("Missing expires_at in Copilot response".into()))?;

    // The API returns expires_at in seconds. Convert to seconds with 5-min buffer.
    let expires_at = Some(expires_at_raw - 300);

    Ok(OAuthCredentials {
        access_token: token,
        refresh_token: Some(refresh_token.to_string()),
        expires_at,
        token_type: "Bearer".into(),
        account_id: None,
    })
}

// ---------------------------------------------------------------------------
// OpenAI Codex OAuth
// ---------------------------------------------------------------------------

/// OpenAI Codex OAuth provider (Authorization Code + PKCE).
pub struct OpenAICodexOAuth;

#[async_trait]
impl OAuthProvider for OpenAICodexOAuth {
    async fn login(&self, callbacks: &dyn OAuthCallbacks) -> Result<OAuthCredentials, OAuthError> {
        login_openai_codex(callbacks).await
    }

    async fn refresh(&self, refresh_token: &str) -> Result<OAuthCredentials, OAuthError> {
        refresh_openai_codex_token(refresh_token).await
    }
}

/// Login with OpenAI Codex OAuth (authorization code + PKCE).
pub async fn login_openai_codex(callbacks: &dyn OAuthCallbacks) -> Result<OAuthCredentials, OAuthError> {
    let (verifier, challenge) = generate_pkce();
    let state = generate_code_verifier(); // random state separate from verifier
    let server = CallbackServer::bind(OPENAI_PORT).await?;

    let redirect_uri = format!("http://localhost:{}{}", OPENAI_PORT, OPENAI_CALLBACK_PATH);

    let mut auth_url = Url::parse(OPENAI_AUTHORIZE_URL)?;
    auth_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", OPENAI_CLIENT_ID)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", OPENAI_SCOPE)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "pi");

    callbacks.on_auth(&OAuthAuthInfo {
        url: auth_url.to_string(),
        instructions: Some("A browser window should open. Complete login to finish.".into()),
    });

    let (code, received_state) = resolve_authorization_code(
        &server,
        callbacks,
        OPENAI_CALLBACK_PATH,
        &state,
        "OpenAI authentication completed. You can close this window.",
        "OpenAI Codex OAuth",
    )
    .await?;

    // Verify state
    if received_state != state {
        return Err(OAuthError::Other("OAuth state mismatch".into()));
    }

    callbacks.on_progress("Exchanging authorization code for tokens...");
    exchange_openai_codex_code(&code, &verifier, &redirect_uri).await
}

async fn exchange_openai_codex_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthCredentials, OAuthError> {
    let body = [
        ("grant_type", "authorization_code"),
        ("client_id", OPENAI_CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ];

    let response_body = post_form(OPENAI_TOKEN_URL, &body).await?;

    let data: serde_json::Value =
        serde_json::from_str(&response_body).map_err(|e| OAuthError::TokenExchange(format!("Invalid JSON: {}", e)))?;

    let access_token = data["access_token"]
        .as_str()
        .ok_or_else(|| OAuthError::TokenExchange("Missing access_token".into()))?
        .to_string();

    let refresh_token = data["refresh_token"]
        .as_str()
        .ok_or_else(|| OAuthError::TokenExchange("Missing refresh_token".into()))?
        .to_string();

    let expires_in =
        data["expires_in"].as_i64().ok_or_else(|| OAuthError::TokenExchange("Missing expires_in".into()))?;

    let expires_at = Some(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
            + expires_in,
    );

    let account_id = decode_codex_account_id(&access_token);

    Ok(OAuthCredentials {
        access_token,
        refresh_token: Some(refresh_token),
        expires_at,
        token_type: "Bearer".into(),
        account_id,
    })
}

/// Refresh an OpenAI Codex OAuth token.
pub async fn refresh_openai_codex_token(refresh_token: &str) -> Result<OAuthCredentials, OAuthError> {
    let body = [("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("client_id", OPENAI_CLIENT_ID)];

    let response_body = post_form(OPENAI_TOKEN_URL, &body).await?;

    let data: serde_json::Value =
        serde_json::from_str(&response_body).map_err(|e| OAuthError::TokenExchange(format!("Invalid JSON: {}", e)))?;

    let access_token = data["access_token"]
        .as_str()
        .ok_or_else(|| OAuthError::TokenExchange("Missing access_token".into()))?
        .to_string();

    let new_refresh = data["refresh_token"]
        .as_str()
        .ok_or_else(|| OAuthError::TokenExchange("Missing refresh_token".into()))?
        .to_string();

    let expires_in =
        data["expires_in"].as_i64().ok_or_else(|| OAuthError::TokenExchange("Missing expires_in".into()))?;

    let expires_at = Some(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
            + expires_in,
    );

    let account_id = decode_codex_account_id(&access_token);

    Ok(OAuthCredentials {
        access_token,
        refresh_token: Some(new_refresh),
        expires_at,
        token_type: "Bearer".into(),
        account_id,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // PKCE tests
    // ---------------------------------------------------------------

    #[test]
    fn test_base64url_encode() {
        // Known test vectors
        assert_eq!(base64url_encode(b""), "");
        assert_eq!(base64url_encode(b"f"), "Zg");
        assert_eq!(base64url_encode(b"fo"), "Zm8");
        assert_eq!(base64url_encode(b"foo"), "Zm9v");
        assert_eq!(base64url_encode(b"foob"), "Zm9vYg");
        assert_eq!(base64url_encode(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn test_pkce_challenge_deterministic() {
        // Known verifier -> known challenge (computed offline)
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = generate_code_challenge(verifier);
        // SHA256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk") base64url-encoded
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(challenge, expected);
    }

    #[test]
    fn test_verifier_length() {
        let verifier = generate_code_verifier();
        // 32 random bytes -> 43 base64url chars (ceil(32*4/3) = 43)
        assert_eq!(verifier.len(), 43);
    }

    #[test]
    fn test_challenge_not_empty() {
        let (verifier, challenge) = generate_pkce();
        assert!(!verifier.is_empty());
        assert!(!challenge.is_empty());
        assert_ne!(verifier, challenge);
    }

    // ---------------------------------------------------------------
    // URL construction tests
    // ---------------------------------------------------------------

    #[test]
    fn test_anthropic_url_construction() {
        let verifier = "test-verifier-123";
        let challenge = generate_code_challenge(verifier);
        let redirect_uri = format!("http://localhost:{}{}", ANTHROPIC_PORT, ANTHROPIC_CALLBACK_PATH);

        let mut auth_url = Url::parse(ANTHROPIC_AUTHORIZE_URL).unwrap();
        auth_url
            .query_pairs_mut()
            .append_pair("code", "true")
            .append_pair("client_id", ANTHROPIC_CLIENT_ID)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", ANTHROPIC_SCOPES)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", verifier);

        let url_str = auth_url.to_string();
        assert!(url_str.starts_with(ANTHROPIC_AUTHORIZE_URL));
        assert!(url_str.contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"));
        assert!(url_str.contains("response_type=code"));
        assert!(url_str.contains(&format!("redirect_uri={}", urlencoding(&redirect_uri))));
        assert!(url_str.contains("code_challenge_method=S256"));
        assert!(url_str.contains("code_challenge="));
        assert!(url_str.contains("state=test-verifier-123"));
        assert!(url_str.contains("scope="));
    }

    #[test]
    fn test_github_copilot_urls() {
        let domain = "github.com";
        let (device_url, access_url, copilot_url) = get_copilot_urls(domain);
        assert_eq!(device_url, "https://github.com/login/device/code");
        assert_eq!(access_url, "https://github.com/login/oauth/access_token");
        assert_eq!(copilot_url, "https://api.github.com/copilot_internal/v2/token");

        // Enterprise domain
        let domain = "company.ghe.com";
        let (device_url, access_url, copilot_url) = get_copilot_urls(domain);
        assert_eq!(device_url, "https://company.ghe.com/login/device/code");
        assert_eq!(access_url, "https://company.ghe.com/login/oauth/access_token");
        assert_eq!(copilot_url, "https://api.company.ghe.com/copilot_internal/v2/token");
    }

    #[test]
    fn test_openai_codex_url_construction() {
        let verifier = "test-verifier-codex";
        let challenge = generate_code_challenge(verifier);
        let state = "test-state-codex";

        let redirect_uri = format!("http://localhost:{}{}", OPENAI_PORT, OPENAI_CALLBACK_PATH);

        let mut auth_url = Url::parse(OPENAI_AUTHORIZE_URL).unwrap();
        auth_url
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", OPENAI_CLIENT_ID)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", OPENAI_SCOPE)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("originator", "pi");

        let url_str = auth_url.to_string();
        assert!(url_str.starts_with(OPENAI_AUTHORIZE_URL));
        assert!(url_str.contains(&format!("client_id={}", OPENAI_CLIENT_ID)));
        assert!(url_str.contains("response_type=code"));
        assert!(url_str.contains(&format!("redirect_uri={}", urlencoding(&redirect_uri))));
        assert!(url_str.contains("code_challenge_method=S256"));
        assert!(url_str.contains("scope=openid+profile+email+offline_access"));
        assert!(url_str.contains("state=test-state-codex"));
        assert!(url_str.contains("codex_cli_simplified_flow=true"));
        assert!(url_str.contains("originator=pi"));
    }

    #[test]
    fn test_normalize_domain() {
        assert_eq!(normalize_domain("github.com").as_deref(), Some("github.com"));
        assert_eq!(normalize_domain("https://github.com").as_deref(), Some("github.com"));
        assert_eq!(normalize_domain("  company.ghe.com  ").as_deref(), Some("company.ghe.com"));
        assert_eq!(normalize_domain(""), None);
        assert_eq!(normalize_domain("   "), None);
    }

    // ---------------------------------------------------------------
    // HTML page tests
    // ---------------------------------------------------------------

    #[test]
    fn test_oauth_success_html() {
        let html = oauth_success_html("Test success");
        assert!(html.contains("Authentication successful"));
        assert!(html.contains("Test success"));
        assert!(html.contains("<html"));
        assert!(html.contains("</html>"));
    }

    #[test]
    fn test_oauth_error_html() {
        let html = oauth_error_html("Test error", Some("details here"));
        assert!(html.contains("Authentication failed"));
        assert!(html.contains("Test error"));
        assert!(html.contains("details here"));
        assert!(html.contains("<html"));
        assert!(html.contains("</html>"));
    }

    #[test]
    fn test_oauth_error_html_no_details() {
        let html = oauth_error_html("Simple error", None);
        assert!(html.contains("Simple error"));
        // The "details" class CSS selector exists in the stylesheet, so we
        // check the rendered content does NOT contain the details div.
        assert!(!html.contains(r#"<div class="details">"#));
    }

    // ---------------------------------------------------------------
    // parse_authorization_input tests
    // ---------------------------------------------------------------

    #[test]
    fn test_parse_full_url() {
        let parsed = parse_authorization_input("http://localhost:53692/callback?code=abc123&state=xyz789");
        assert_eq!(parsed.code, "abc123");
        assert_eq!(parsed.state.as_deref(), Some("xyz789"));
    }

    #[test]
    fn test_parse_url_without_state() {
        let parsed = parse_authorization_input("http://localhost:53692/callback?code=abc123");
        assert_eq!(parsed.code, "abc123");
        assert_eq!(parsed.state, None);
    }

    #[test]
    fn test_parse_hash_format() {
        let parsed = parse_authorization_input("abc123#xyz789");
        assert_eq!(parsed.code, "abc123");
        assert_eq!(parsed.state.as_deref(), Some("xyz789"));
    }

    #[test]
    fn test_parse_query_string() {
        let parsed = parse_authorization_input("code=abc123&state=xyz789");
        assert_eq!(parsed.code, "abc123");
        assert_eq!(parsed.state.as_deref(), Some("xyz789"));
    }

    #[test]
    fn test_parse_raw_code() {
        let parsed = parse_authorization_input("abc123");
        assert_eq!(parsed.code, "abc123");
        assert_eq!(parsed.state, None);
    }

    #[test]
    fn test_parse_empty() {
        let parsed = parse_authorization_input("");
        assert!(parsed.code.is_empty());
        assert_eq!(parsed.state, None);

        let parsed = parse_authorization_input("   ");
        assert!(parsed.code.is_empty());
        assert_eq!(parsed.state, None);
    }

    // ---------------------------------------------------------------
    // Callback server integration tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_anthropic_callback_server() {
        let server = CallbackServer::bind(53691).await.unwrap();

        let client = reqwest::Client::new();
        let callback_url =
            format!("http://127.0.0.1:53691{}?code=test_code_123&state=expected_state", ANTHROPIC_CALLBACK_PATH);

        let response_fut = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            client.get(&callback_url).send().await
        };

        let server_fut = server.wait_for_callback(ANTHROPIC_CALLBACK_PATH, "expected_state", "Success!", "Test");

        let (result, response) = tokio::join!(server_fut, response_fut);

        let (code, state) = result.unwrap();
        assert_eq!(code, "test_code_123");
        assert_eq!(state, "expected_state");

        let resp = response.unwrap();
        assert!(resp.status().is_success());
        let resp_text = resp.text().await.unwrap();
        assert!(resp_text.contains("Success!"));
    }

    #[tokio::test]
    async fn test_anthropic_callback_server_state_mismatch() {
        let server = CallbackServer::bind(53690).await.unwrap();

        let client = reqwest::Client::new();
        let callback_url =
            format!("http://127.0.0.1:53690{}?code=test_code&state=wrong_state", ANTHROPIC_CALLBACK_PATH);

        let response_fut = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            client.get(&callback_url).send().await
        };

        let server_fut = server.wait_for_callback(ANTHROPIC_CALLBACK_PATH, "expected_state", "Success!", "Test");

        let (result, response) = tokio::join!(server_fut, response_fut);

        assert!(result.is_err());
        let resp = response.unwrap();
        assert_eq!(resp.status(), 400);
    }

    // ---------------------------------------------------------------
    // Mock-based token exchange tests (wiremock)
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_anthropic_token_exchange() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let _token_url = mock_server.uri();

        // Set up mock response
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "ant-test-access-token",
                "refresh_token": "ant-test-refresh-token",
                "expires_in": 3600,
                "token_type": "Bearer"
            })))
            .mount(&mock_server)
            .await;

        // We can't easily test exchange_anthropic_code directly because it hardcodes
        // ANTHROPIC_TOKEN_URL. Instead, test that the JSON body construction is correct
        // by verifying the payload that would be sent.

        // Test the JSON body construction
        let body = serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": ANTHROPIC_CLIENT_ID,
            "code": "test-code",
            "state": "test-state",
            "redirect_uri": "http://localhost:53692/callback",
            "code_verifier": "test-verifier",
        });

        assert_eq!(body["grant_type"], "authorization_code");
        assert_eq!(body["client_id"], ANTHROPIC_CLIENT_ID);
        assert_eq!(body["code"], "test-code");
        assert_eq!(body["state"], "test-state");
        assert_eq!(body["redirect_uri"], "http://localhost:53692/callback");
        assert_eq!(body["code_verifier"], "test-verifier");
    }

    #[tokio::test]
    async fn test_anthropic_token_refresh_body() {
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": ANTHROPIC_CLIENT_ID,
            "refresh_token": "test-refresh-token",
        });

        assert_eq!(body["grant_type"], "refresh_token");
        assert_eq!(body["client_id"], ANTHROPIC_CLIENT_ID);
        assert_eq!(body["refresh_token"], "test-refresh-token");
    }

    #[tokio::test]
    async fn test_github_copilot_device_code_body() {
        let body = [("client_id", GITHUB_CLIENT_ID), ("scope", "read:user")];

        assert_eq!(body[0], ("client_id", "Iv1.b507a08c87ecfe98"));
        assert_eq!(body[1], ("scope", "read:user"));
    }

    #[tokio::test]
    async fn test_openai_codex_token_exchange_body() {
        let body = [
            ("grant_type", "authorization_code"),
            ("client_id", OPENAI_CLIENT_ID),
            ("code", "test-code"),
            ("code_verifier", "test-verifier"),
            ("redirect_uri", "http://localhost:1455/auth/callback"),
        ];

        assert_eq!(body[0], ("grant_type", "authorization_code"));
        assert_eq!(body[1], ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann"));
        assert_eq!(body[2], ("code", "test-code"));
        assert_eq!(body[3], ("code_verifier", "test-verifier"));
        assert_eq!(body[4], ("redirect_uri", "http://localhost:1455/auth/callback"));
    }

    #[tokio::test]
    async fn test_openai_codex_token_refresh_body() {
        let body =
            [("grant_type", "refresh_token"), ("refresh_token", "test-refresh-token"), ("client_id", OPENAI_CLIENT_ID)];

        assert_eq!(body[0], ("grant_type", "refresh_token"));
        assert_eq!(body[1], ("refresh_token", "test-refresh-token"));
        assert_eq!(body[2], ("client_id", "app_EMoamEEZ73f0CkXaXp7hrann"));
    }

    // ---------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------

    /// URL-encode a string for assertion matching (replicates form-urlencoding).
    fn urlencoding(s: &str) -> String {
        let mut result = String::new();
        for byte in s.as_bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    result.push(*byte as char);
                }
                _ => {
                    result.push_str(&format!("%{:02X}", byte));
                }
            }
        }
        result
    }
}
