//! SignFlow's helper API (app/api/helper in SignFlow): the only calls the
//! helper makes to SignFlow. The address is built in (config::SIGNFLOW_URL).
//! The edit key travels only in the Authorization header, never in a URL;
//! nothing here logs a pass, a key, a token or a signed link.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Why a call didn't give an answer to act on.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// SignFlow said no, in a sentence to show her (an ended edit, no access,
    /// a used or expired link, a file too big…). Not retried.
    Refused { status: u16, message: String },
    /// Her save was over a version somebody replaced since: she chooses.
    Stale(StaleInfo),
    /// No answer, or SignFlow was briefly unavailable: keep the file, retry.
    Unreachable(String),
    /// An answer the helper doesn't understand. Treated like Unreachable.
    Unexpected(String),
}

impl ApiError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, ApiError::Unreachable(_) | ApiError::Unexpected(_))
    }
    /// The sentence to show her.
    pub fn message(&self) -> String {
        match self {
            ApiError::Refused { message, .. } => message.clone(),
            ApiError::Stale(_) => "This file changed in SignFlow since you opened it.".into(),
            ApiError::Unreachable(_) | ApiError::Unexpected(_) => "Can't reach SignFlow right now. Your file is safe on this computer; trying again.".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaleInfo {
    pub changed_by: Option<String>,
    pub changed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub id: String,
    pub name: String,
    pub size_bytes: u64,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exchange {
    pub key: String,
    pub key_expires_at: String,
    pub heartbeat_seconds: u64,
    pub lapse_seconds: u64,
    pub file: FileInfo,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadLink {
    pub url: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadToken {
    pub client_token: String,
    pub pathname: String,
    pub maximum_size_in_bytes: u64,
    pub valid_until: String,
}

/// What she chose when her save was stale (SignFlow's own words for them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Choice {
    #[serde(rename = "keep-theirs")]
    KeepTheirs,
    #[serde(rename = "upload-mine-as-newest")]
    UploadMineAsNewest,
    #[serde(rename = "save-mine-as-copy")]
    SaveMineAsCopy,
}

impl Choice {
    pub fn parse(s: &str) -> Option<Choice> {
        match s {
            "keep-theirs" => Some(Choice::KeepTheirs),
            "upload-mine-as-newest" => Some(Choice::UploadMineAsNewest),
            "save-mine-as-copy" => Some(Choice::SaveMineAsCopy),
            _ => None,
        }
    }
}

/// What became of a save.
#[derive(Debug, Clone, PartialEq)]
pub enum Saved {
    Replaced { version: String },
    KeptTheirs { version: String },
    Copy { copy_id: String, copy_name: String },
}

pub struct Api {
    http: reqwest::Client,
    base: String,
}

impl Api {
    pub fn new(base: &str) -> Api {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .user_agent(concat!("SignFlowHelper/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        Api { http, base: base.trim_end_matches('/').to_string() }
    }

    async fn post(&self, route: &str, key: Option<&str>, body: Option<Value>) -> Result<Value, ApiError> {
        let mut req = self.http.post(format!("{}/api/helper/{route}", self.base));
        if let Some(key) = key {
            req = req.bearer_auth(key);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await.map_err(|e| ApiError::Unreachable(short(&e)))?;
        let status = res.status().as_u16();
        let text = res.text().await.map_err(|e| ApiError::Unreachable(short(&e)))?;
        let json: Value = serde_json::from_str(&text).map_err(|_| {
            if status >= 500 { ApiError::Unreachable(format!("status {status}")) } else { ApiError::Unexpected(format!("status {status}, not JSON")) }
        })?;
        if status >= 500 {
            return Err(ApiError::Unreachable(format!("status {status}")));
        }
        if json.get("ok").and_then(Value::as_bool) == Some(true) && (200..300).contains(&status) {
            return Ok(json);
        }
        if status == 409 {
            if let Some(stale) = json.get("stale") {
                if let Ok(info) = serde_json::from_value::<StaleInfo>(stale.clone()) {
                    return Err(ApiError::Stale(info));
                }
            }
        }
        let message = json
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("SignFlow refused this ({status})."));
        if (400..500).contains(&status) {
            return Err(ApiError::Refused { status, message });
        }
        Err(ApiError::Unexpected(format!("status {status}")))
    }

    fn parse<T: for<'de> Deserialize<'de>>(json: Value) -> Result<T, ApiError> {
        serde_json::from_value(json).map_err(|e| ApiError::Unexpected(format!("answer: {e}")))
    }

    /// The link's pass for an edit key (once; about 2 minutes).
    pub async fn exchange(&self, pass: &str) -> Result<Exchange, ApiError> {
        Self::parse(self.post("exchange-pass", None, Some(serde_json::json!({ "pass": pass }))).await?)
    }

    /// A 5-minute link to the file as it is now.
    pub async fn download_link(&self, key: &str) -> Result<DownloadLink, ApiError> {
        Self::parse(self.post("download-link", Some(key), None).await?)
    }

    /// A token for one new file, at a path SignFlow chooses.
    pub async fn upload_token(&self, key: &str) -> Result<UploadToken, ApiError> {
        Self::parse(self.post("upload-token", Some(key), None).await?)
    }

    /// Her save: the uploaded file becomes the new version, or SignFlow says
    /// it's stale; `choice` is her answer to a stale save.
    pub async fn replace(&self, key: &str, url: &str, choice: Option<Choice>) -> Result<Saved, ApiError> {
        let mut body = serde_json::json!({ "url": url });
        if let Some(c) = choice {
            body["choice"] = serde_json::to_value(c).unwrap();
        }
        let json = self.post("replace", Some(key), Some(body)).await?;
        match json.get("saved").and_then(Value::as_str) {
            Some("replaced") => Ok(Saved::Replaced { version: str_field(&json, "version")? }),
            Some("kept-theirs") => Ok(Saved::KeptTheirs { version: str_field(&json, "version")? }),
            Some("copy") => Ok(Saved::Copy { copy_id: str_field(&json, "copyId")?, copy_name: str_field(&json, "copyName")? }),
            _ => Err(ApiError::Unexpected("answer: saved".into())),
        }
    }

    /// Still here: keeps the lock.
    pub async fn heartbeat(&self, key: &str) -> Result<(), ApiError> {
        self.post("heartbeat", Some(key), None).await.map(|_| ())
    }

    /// Done: the edit and the lock end.
    pub async fn done(&self, key: &str) -> Result<(), ApiError> {
        self.post("done", Some(key), None).await.map(|_| ())
    }
}

fn str_field(json: &Value, name: &str) -> Result<String, ApiError> {
    json.get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ApiError::Unexpected(format!("answer: {name}")))
}

/// An error's kind without any URL in it (a URL may carry a signature).
fn short(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "couldn't connect"
    } else if e.is_request() {
        "request failed"
    } else if e.is_body() || e.is_decode() {
        "answer cut off"
    } else {
        "network error"
    };
    kind.to_string()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Json, Router,
    };
    use std::sync::{Arc, Mutex};

    /// A tiny stand-in for SignFlow's helper API: records what it was sent.
    #[derive(Default)]
    pub struct Seen {
        pub calls: Vec<(String, Option<String>, Option<Value>)>,
    }

    async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    fn auth(h: &HeaderMap) -> Option<String> {
        h.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string)
    }

    const PASS: &str = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_abcde";
    const KEY: &str = "KkKkKkKkKkKkKkKkKkKkKkKkKkKkKkKkKkKkKkKkKkK";

    fn app(seen: Arc<Mutex<Seen>>) -> Router {
        let rec = |route: &'static str| {
            move |State(seen): State<Arc<Mutex<Seen>>>, h: HeaderMap, body: Option<Json<Value>>| async move {
                seen.lock().unwrap().calls.push((route.to_string(), auth(&h), body.as_ref().map(|b| b.0.clone())));
                let key_ok = auth(&h).as_deref() == Some(&format!("Bearer {KEY}"));
                match route {
                    "exchange-pass" => {
                        let pass = body.as_ref().and_then(|b| b.0.get("pass")).and_then(Value::as_str).unwrap_or("");
                        if pass == PASS {
                            (StatusCode::OK, Json(serde_json::json!({ "ok": true, "key": KEY, "keyExpiresAt": "2026-11-12T20:00:00.000Z", "heartbeatSeconds": 60, "lapseSeconds": 900, "file": { "id": "f1", "name": "Concept.fs", "sizeBytes": 1234, "version": "v-1" } })))
                        } else if pass == "used" {
                            (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "ok": false, "error": "This link was already used. Open the file again from SignFlow." })))
                        } else {
                            (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "ok": false, "error": "This link isn't valid. Open the file again from SignFlow." })))
                        }
                    }
                    _ if !key_ok => (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "ok": false, "error": "This edit isn't open any more. Open the file again from SignFlow." }))),
                    "download-link" => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "url": "https://s.private.blob.vercel-storage.com/jobs/j/u/Concept.fs?sig=x", "expiresAt": "x", "fileName": "Concept.fs", "sizeBytes": 1234, "version": "v-1" }))),
                    "upload-token" => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "clientToken": "vercel_blob_client_store1_abc", "pathname": "jobs/j/u/Concept.fs", "maximumSizeInBytes": 524288000u64, "validUntil": "x" }))),
                    "replace" => {
                        let b = body.map(|b| b.0).unwrap_or_default();
                        match b.get("choice").and_then(Value::as_str) {
                            None if b.get("url").and_then(Value::as_str) == Some("stale-url") => (StatusCode::CONFLICT, Json(serde_json::json!({ "ok": false, "error": "This file changed since you opened it", "stale": { "changedBy": "Dana", "changedAt": "2026-11-12T19:00:00.000Z" }, "choices": ["keep-theirs", "upload-mine-as-newest", "save-mine-as-copy"] }))),
                            None => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "saved": "replaced", "version": "v-2" }))),
                            Some("keep-theirs") => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "saved": "kept-theirs", "version": "v-3" }))),
                            Some("upload-mine-as-newest") => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "saved": "replaced", "version": "v-4" }))),
                            Some("save-mine-as-copy") => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "saved": "copy", "copyId": "c1", "copyName": "Concept (copy).fs" }))),
                            _ => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "ok": false, "error": "Choose keep theirs, upload mine as newest, or save mine as a copy." }))),
                        }
                    }
                    "heartbeat" | "done" => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))),
                    "boom" => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "x" }))),
                    _ => (StatusCode::NOT_FOUND, Json(serde_json::json!({ "ok": false, "error": "?" }))),
                }
            }
        };
        Router::new()
            .route("/api/helper/exchange-pass", post(rec("exchange-pass")))
            .route("/api/helper/download-link", post(rec("download-link")))
            .route("/api/helper/upload-token", post(rec("upload-token")))
            .route("/api/helper/replace", post(rec("replace")))
            .route("/api/helper/heartbeat", post(rec("heartbeat")))
            .route("/api/helper/done", post(rec("done")))
            .with_state(seen)
    }

    #[tokio::test]
    async fn exchanges_a_pass_and_sends_the_key_only_in_the_header() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let api = Api::new(&serve(app(seen.clone())).await);
        let ex = api.exchange(PASS).await.unwrap();
        assert_eq!(ex.key, KEY);
        assert_eq!(ex.file, FileInfo { id: "f1".into(), name: "Concept.fs".into(), size_bytes: 1234, version: "v-1".into() });
        assert_eq!((ex.heartbeat_seconds, ex.lapse_seconds), (60, 900));
        let d = api.download_link(KEY).await.unwrap();
        assert_eq!(d.file_name, "Concept.fs");
        let t = api.upload_token(KEY).await.unwrap();
        assert_eq!(t.maximum_size_in_bytes, 500 * 1024 * 1024);
        api.heartbeat(KEY).await.unwrap();
        api.done(KEY).await.unwrap();
        let calls = &seen.lock().unwrap().calls;
        assert_eq!(calls[0].1, None, "the exchange carries no key");
        assert_eq!(calls[0].2.as_ref().unwrap()["pass"], PASS, "the pass is in the body");
        for (route, auth, _) in &calls[1..] {
            assert_eq!(auth.as_deref(), Some(format!("Bearer {KEY}").as_str()), "{route}: key in the header");
        }
    }

    #[tokio::test]
    async fn a_used_or_unknown_pass_is_refused_with_signflows_words() {
        let api = Api::new(&serve(app(Arc::new(Mutex::new(Seen::default())))).await);
        assert_eq!(
            api.exchange("used").await,
            Err(ApiError::Refused { status: 401, message: "This link was already used. Open the file again from SignFlow.".into() })
        );
        let e = api.exchange("nope").await.unwrap_err();
        assert!(!e.is_retryable());
        assert_eq!(e.message(), "This link isn't valid. Open the file again from SignFlow.");
    }

    #[tokio::test]
    async fn a_wrong_key_is_refused() {
        let api = Api::new(&serve(app(Arc::new(Mutex::new(Seen::default())))).await);
        let e = api.heartbeat("not-the-key").await.unwrap_err();
        assert!(matches!(e, ApiError::Refused { status: 401, .. }));
    }

    #[tokio::test]
    async fn a_stale_save_and_the_three_choices() {
        let api = Api::new(&serve(app(Arc::new(Mutex::new(Seen::default())))).await);
        assert_eq!(api.replace(KEY, "fresh-url", None).await, Ok(Saved::Replaced { version: "v-2".into() }));
        assert_eq!(
            api.replace(KEY, "stale-url", None).await,
            Err(ApiError::Stale(StaleInfo { changed_by: Some("Dana".into()), changed_at: Some("2026-11-12T19:00:00.000Z".into()) }))
        );
        assert_eq!(api.replace(KEY, "stale-url", Some(Choice::KeepTheirs)).await, Ok(Saved::KeptTheirs { version: "v-3".into() }));
        assert_eq!(api.replace(KEY, "stale-url", Some(Choice::UploadMineAsNewest)).await, Ok(Saved::Replaced { version: "v-4".into() }));
        assert_eq!(api.replace(KEY, "stale-url", Some(Choice::SaveMineAsCopy)).await, Ok(Saved::Copy { copy_id: "c1".into(), copy_name: "Concept (copy).fs".into() }));
        assert_eq!(Choice::parse("upload-mine-as-newest"), Some(Choice::UploadMineAsNewest));
        assert_eq!(Choice::parse("delete-theirs"), None);
    }

    #[tokio::test]
    async fn no_answer_is_retried_not_refused() {
        // Nothing listens on this port.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let api = Api::new(&format!("http://127.0.0.1:{port}"));
        let e = api.heartbeat(KEY).await.unwrap_err();
        assert!(e.is_retryable(), "{e:?}");
        assert!(e.message().contains("Your file is safe"));
    }
}
