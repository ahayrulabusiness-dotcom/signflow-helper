//! Moving the file's bytes, never through SignFlow: downloads through the
//! 5-minute signed link SignFlow hands back, and uploads straight to Vercel
//! Blob with SignFlow's single-use token (one new file, at the path SignFlow
//! chose, 500MB at most, one hour, no overwriting). The helper never holds a
//! storage key: the token is all it has, and it is good for that one file.
//!
//! Both directions stream. An upload over 10MB goes in 8MB parts, as
//! @vercel/blob does, so a 500MB file never sits in memory: one part at a
//! time. The wire format is @vercel/blob 2.7's (x-api-version 12).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

use crate::api::UploadToken;
use crate::config::{self, BLOB_API_VERSION, MAX_FILE_BYTES, MULTIPART_THRESHOLD, PART_SIZE};

/// Bytes sent (or received) so far, and the total.
pub type Progress = Arc<dyn Fn(u64, u64) + Send + Sync>;

#[derive(Debug, Clone, PartialEq)]
pub enum BlobError {
    /// Bigger than SignFlow takes: not sent at all.
    TooBig { size: u64, max: u64 },
    /// The store or the link said no: a fresh token or link is needed.
    Refused(String),
    /// No answer, or the store was briefly unavailable: retry later.
    Unreachable(String),
    /// The download link isn't a file-store link: not fetched.
    NotAllowed,
    /// Reading or writing the file on this computer failed.
    Disk(String),
}

impl BlobError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, BlobError::Unreachable(_))
    }
}

/// JavaScript's encodeURIComponent, for x-mpu-key.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'!').remove(b'~').remove(b'*').remove(b'\'').remove(b'(').remove(b')');

pub struct BlobClient {
    http: reqwest::Client,
    api: String,
}

/// The store id inside a client token: vercel_blob_client_<storeId>_<…>.
pub fn store_id_of(client_token: &str) -> Option<&str> {
    let mut parts = client_token.splitn(5, '_');
    match (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("vercel"), Some("blob"), Some("client"), Some(id), Some(_)) if !id.is_empty() => Some(id),
        _ => None,
    }
}

impl BlobClient {
    pub fn new(api: &str) -> BlobClient {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            // A part or a whole small file; generous for a slow line.
            .timeout(Duration::from_secs(15 * 60))
            .user_agent(concat!("SignFlowHelper/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        BlobClient { http, api: api.trim_end_matches('/').to_string() }
    }

    fn headers(&self, token: &UploadToken) -> Result<HeaderMap, BlobError> {
        let store = store_id_of(&token.client_token).ok_or_else(|| BlobError::Refused("SignFlow's upload token couldn't be read.".into()))?;
        let mut h = HeaderMap::new();
        let v = |s: &str| HeaderValue::from_str(s).map_err(|_| BlobError::Refused("SignFlow's upload token couldn't be read.".into()));
        h.insert(AUTHORIZATION, v(&format!("Bearer {}", token.client_token))?);
        h.insert("x-api-version", v(BLOB_API_VERSION)?);
        h.insert("x-vercel-blob-store-id", v(store)?);
        h.insert("x-vercel-blob-access", v("private")?);
        h.insert("x-content-type", v("application/octet-stream")?);
        Ok(h)
    }

    fn endpoint(&self, route: &str, pathname: &str) -> String {
        let query = url::form_urlencoded::Serializer::new(String::new()).append_pair("pathname", pathname).finish();
        format!("{}{route}?{query}", self.api)
    }

    /// Uploads the file at `path` as the token's one new file; the stored
    /// file's URL, which SignFlow then swaps in.
    pub async fn upload(&self, token: &UploadToken, path: &Path, progress: Progress) -> Result<String, BlobError> {
        let size = tokio::fs::metadata(path).await.map_err(disk)?.len();
        let max = token.maximum_size_in_bytes.min(MAX_FILE_BYTES);
        if size > max {
            return Err(BlobError::TooBig { size, max });
        }
        let headers = self.headers(token)?;
        progress(0, size);
        if size <= MULTIPART_THRESHOLD {
            self.put_whole(token, path, size, headers, progress).await
        } else {
            self.put_in_parts(token, path, size, headers, progress).await
        }
    }

    async fn put_whole(&self, token: &UploadToken, path: &Path, size: u64, headers: HeaderMap, progress: Progress) -> Result<String, BlobError> {
        let file = tokio::fs::File::open(path).await.map_err(disk)?;
        let mut sent = 0u64;
        let p = progress.clone();
        let stream = tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024).map(move |chunk| {
            if let Ok(c) = &chunk {
                sent += c.len() as u64;
                p(sent, size);
            }
            chunk
        });
        let res = self
            .http
            .put(self.endpoint("/", &token.pathname))
            .headers(headers)
            .header("x-content-length", size)
            .header(CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(net)?;
        let json = answer(res).await?;
        url_of(&json)
    }

    async fn put_in_parts(&self, token: &UploadToken, path: &Path, size: u64, headers: HeaderMap, progress: Progress) -> Result<String, BlobError> {
        let pathname = &token.pathname;
        let created = self.call_mpu(pathname, &headers, &[("x-mpu-action", "create".into())], None).await?;
        let key = str_of(&created, "key")?;
        let upload_id = str_of(&created, "uploadId")?;
        let encoded_key = utf8_percent_encode(&key, URI_COMPONENT).to_string();

        let mut file = tokio::fs::File::open(path).await.map_err(disk)?;
        let mut parts: Vec<Value> = Vec::new();
        let mut sent = 0u64;
        let mut number = 1u32;
        loop {
            // One part in memory at a time.
            let mut buf = vec![0u8; PART_SIZE];
            let mut filled = 0;
            while filled < PART_SIZE {
                let n = file.read(&mut buf[filled..]).await.map_err(disk)?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break;
            }
            buf.truncate(filled);
            let body = Bytes::from(buf);
            let extra = [
                ("x-mpu-action", "upload".to_string()),
                ("x-mpu-key", encoded_key.clone()),
                ("x-mpu-upload-id", upload_id.clone()),
                ("x-mpu-part-number", number.to_string()),
            ];
            let done = retry(4, || self.call_mpu(pathname, &headers, &extra, Some(body.clone()))).await?;
            parts.push(serde_json::json!({ "partNumber": number, "etag": str_of(&done, "etag")? }));
            sent += filled as u64;
            progress(sent, size);
            number += 1;
            if filled < PART_SIZE {
                break;
            }
        }
        if sent != size {
            return Err(BlobError::Disk("The file changed while it was being sent.".into()));
        }
        let extra = [
            ("x-mpu-action", "complete".to_string()),
            ("x-mpu-key", encoded_key),
            ("x-mpu-upload-id", upload_id),
        ];
        let body = Bytes::from(serde_json::to_vec(&parts).unwrap());
        let done = retry(4, || self.call_mpu(pathname, &headers, &extra, Some(body.clone()))).await?;
        url_of(&done)
    }

    async fn call_mpu(&self, pathname: &str, headers: &HeaderMap, extra: &[(&str, String)], body: Option<Bytes>) -> Result<Value, BlobError> {
        let mut req = self.http.post(self.endpoint("/mpu", pathname)).headers(headers.clone());
        for (k, v) in extra {
            req = req.header(*k, v.as_str());
        }
        if let Some(b) = body {
            let is_json = extra.iter().any(|(k, v)| *k == "x-mpu-action" && v == "complete");
            if is_json {
                req = req.header(CONTENT_TYPE, "application/json");
            }
            req = req.header("x-content-length", b.len()).body(b);
        }
        answer(req.send().await.map_err(net)?).await
    }

    /// Streams the signed link's file to `dest` (a new file; the caller
    /// moves it into place). Only file-store links are fetched.
    pub async fn download(&self, link: &str, dest: &Path, expected: Option<u64>, progress: Progress) -> Result<u64, BlobError> {
        let url = Url::parse(link).map_err(|_| BlobError::NotAllowed)?;
        if !config::download_url_allowed(&url) {
            return Err(BlobError::NotAllowed);
        }
        let res = self.http.get(url).send().await.map_err(net)?;
        let status = res.status();
        if status.is_server_error() {
            return Err(BlobError::Unreachable(format!("status {}", status.as_u16())));
        }
        if !status.is_success() {
            return Err(BlobError::Refused(format!("The download link was refused ({}).", status.as_u16())));
        }
        let total = res.content_length().or(expected).unwrap_or(0);
        let mut out = tokio::fs::OpenOptions::new().write(true).create_new(true).open(dest).await.map_err(disk)?;
        let mut stream = res.bytes_stream();
        let mut got = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(net)?;
            out.write_all(&chunk).await.map_err(disk)?;
            got += chunk.len() as u64;
            progress(got, total);
        }
        out.flush().await.map_err(disk)?;
        out.sync_all().await.map_err(disk)?;
        if let Some(want) = expected {
            if want != got {
                return Err(BlobError::Unreachable("The download was cut off.".into()));
            }
        }
        Ok(got)
    }
}

async fn retry<F, Fut>(attempts: u32, mut call: F) -> Result<Value, BlobError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Value, BlobError>>,
{
    let mut wait = Duration::from_secs(1);
    let mut left = attempts;
    loop {
        match call().await {
            Err(e) if e.is_retryable() && left > 1 => {
                left -= 1;
                tokio::time::sleep(wait).await;
                wait *= 2;
            }
            other => return other,
        }
    }
}

async fn answer(res: reqwest::Response) -> Result<Value, BlobError> {
    let status = res.status();
    let text = res.text().await.map_err(net)?;
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(json);
    }
    let message = json
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("The file store refused the upload.")
        .to_string();
    if status.is_server_error() || status.as_u16() == 429 {
        Err(BlobError::Unreachable(format!("status {}", status.as_u16())))
    } else {
        Err(BlobError::Refused(message))
    }
}

fn url_of(json: &Value) -> Result<String, BlobError> {
    str_of(json, "url")
}

fn str_of(json: &Value, name: &str) -> Result<String, BlobError> {
    json.get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| BlobError::Unreachable(format!("the store's answer had no {name}")))
}

fn net(e: reqwest::Error) -> BlobError {
    BlobError::Unreachable(if e.is_timeout() { "timed out".into() } else if e.is_connect() { "couldn't connect".into() } else { "network error".into() })
}

fn disk(e: std::io::Error) -> BlobError {
    BlobError::Disk(e.kind().to_string())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use axum::{
        body::Bytes as AxBytes,
        extract::{Query, State},
        http::{HeaderMap as AxHeaders, StatusCode},
        routing::{post, put},
        Json, Router,
    };
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A stand-in for Vercel Blob's upload API: checks what @vercel/blob
    /// checks and keeps what arrives.
    #[derive(Default)]
    pub struct Store {
        pub files: HashMap<String, Vec<u8>>,
        pub parts: HashMap<String, Vec<(u32, Vec<u8>)>>,
        pub headers_seen: Vec<HashMap<String, String>>,
        pub largest_request: usize,
    }

    type Shared = Arc<Mutex<Store>>;

    fn record(store: &Shared, h: &AxHeaders, len: usize) {
        let mut s = store.lock().unwrap();
        s.headers_seen.push(h.iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect());
        s.largest_request = s.largest_request.max(len);
    }

    fn check(h: &AxHeaders) -> Result<(), (StatusCode, Json<Value>)> {
        let ok = h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer vercel_blob_client_store1_payload")
            && h.get("x-api-version").and_then(|v| v.to_str().ok()) == Some("12")
            && h.get("x-vercel-blob-store-id").and_then(|v| v.to_str().ok()) == Some("store1")
            && h.get("x-vercel-blob-access").and_then(|v| v.to_str().ok()) == Some("private");
        if ok { Ok(()) } else { Err((StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": { "code": "forbidden", "message": "Access denied" } })))) }
    }

    async fn put_whole(State(store): State<Shared>, Query(q): Query<HashMap<String, String>>, h: AxHeaders, body: AxBytes) -> (StatusCode, Json<Value>) {
        record(&store, &h, body.len());
        if let Err(e) = check(&h) { return e; }
        let p = q.get("pathname").cloned().unwrap_or_default();
        store.lock().unwrap().files.insert(p.clone(), body.to_vec());
        (StatusCode::OK, Json(serde_json::json!({ "url": format!("https://store1.private.blob.vercel-storage.com/{p}"), "pathname": p })))
    }

    async fn mpu(State(store): State<Shared>, Query(q): Query<HashMap<String, String>>, h: AxHeaders, body: AxBytes) -> (StatusCode, Json<Value>) {
        record(&store, &h, body.len());
        if let Err(e) = check(&h) { return e; }
        let p = q.get("pathname").cloned().unwrap_or_default();
        let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        match get("x-mpu-action").as_str() {
            "create" => (StatusCode::OK, Json(serde_json::json!({ "key": p, "uploadId": "up1" }))),
            "upload" => {
                let n: u32 = get("x-mpu-part-number").parse().unwrap();
                assert_eq!(percent_encoding::percent_decode_str(&get("x-mpu-key")).decode_utf8().unwrap(), p);
                store.lock().unwrap().parts.entry(p).or_default().push((n, body.to_vec()));
                (StatusCode::OK, Json(serde_json::json!({ "etag": format!("e{n}") })))
            }
            "complete" => {
                let list: Vec<Value> = serde_json::from_slice(&body).unwrap();
                let mut s = store.lock().unwrap();
                let mut parts = s.parts.remove(&p).unwrap_or_default();
                parts.sort_by_key(|(n, _)| *n);
                assert_eq!(list.len(), parts.len());
                let whole: Vec<u8> = parts.into_iter().flat_map(|(_, b)| b).collect();
                s.files.insert(p.clone(), whole);
                (StatusCode::OK, Json(serde_json::json!({ "url": format!("https://store1.private.blob.vercel-storage.com/{p}"), "pathname": p })))
            }
            _ => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": { "code": "bad_request", "message": "?" } }))),
        }
    }

    pub async fn serve_store() -> (String, Shared) {
        let store: Shared = Arc::new(Mutex::new(Store::default()));
        let app = Router::new()
            .route("/api/blob/", put(put_whole))
            .route("/api/blob/mpu", post(mpu))
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
            .with_state(store.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/api/blob"), store)
    }

    fn token(max: u64) -> UploadToken {
        UploadToken { client_token: "vercel_blob_client_store1_payload".into(), pathname: "jobs/j1/0b6f/Concept sign.fs".into(), maximum_size_in_bytes: max, valid_until: "x".into() }
    }

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    #[test]
    fn reads_the_store_id_from_the_token() {
        assert_eq!(store_id_of("vercel_blob_client_store1_eyJhIjoxfQ=="), Some("store1"));
        assert_eq!(store_id_of("vercel_blob_client_store1_a_b_c"), Some("store1"));
        assert_eq!(store_id_of("vercel_blob_rw_store1_fake"), None);
        assert_eq!(store_id_of("garbage"), None);
    }

    #[tokio::test]
    async fn a_small_file_goes_in_one_streamed_put() {
        let (api, store) = serve_store().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Concept sign.fs");
        let data = pattern(3 * 1024 * 1024 + 17, 7);
        std::fs::write(&path, &data).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let url = BlobClient::new(&api).upload(&token(MAX_FILE_BYTES), &path, Arc::new(move |a, b| s2.lock().unwrap().push((a, b)))).await.unwrap();
        assert_eq!(url, "https://store1.private.blob.vercel-storage.com/jobs/j1/0b6f/Concept sign.fs");
        let s = store.lock().unwrap();
        assert_eq!(s.files["jobs/j1/0b6f/Concept sign.fs"], data);
        assert_eq!(s.headers_seen[0].get("x-content-length").map(String::as_str), Some(&*data.len().to_string()));
        let p = seen.lock().unwrap();
        assert_eq!(*p.last().unwrap(), (data.len() as u64, data.len() as u64), "progress reaches 100%");
        assert!(p.len() > 3, "progress reported along the way");
    }

    #[tokio::test]
    async fn a_large_file_goes_in_8mb_parts_and_arrives_whole() {
        let (api, store) = serve_store().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Big.fs");
        let data = pattern(3 * PART_SIZE + 12345, 3);
        std::fs::write(&path, &data).unwrap();
        BlobClient::new(&api).upload(&token(MAX_FILE_BYTES), &path, Arc::new(|_, _| {})).await.unwrap();
        let s = store.lock().unwrap();
        assert_eq!(s.files["jobs/j1/0b6f/Concept sign.fs"].len(), data.len());
        assert_eq!(s.files["jobs/j1/0b6f/Concept sign.fs"], data);
        assert!(s.largest_request <= PART_SIZE, "no request bigger than one part: {}", s.largest_request);
        let actions: Vec<_> = s.headers_seen.iter().filter_map(|h| h.get("x-mpu-action").cloned()).collect();
        assert_eq!(actions, ["create", "upload", "upload", "upload", "upload", "complete"]);
    }

    #[tokio::test]
    async fn too_big_is_refused_before_sending_anything() {
        let (api, store) = serve_store().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Huge.fs");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();
        let r = BlobClient::new(&api).upload(&token(MAX_FILE_BYTES), &path, Arc::new(|_, _| {})).await;
        assert_eq!(r, Err(BlobError::TooBig { size: MAX_FILE_BYTES + 1, max: MAX_FILE_BYTES }));
        assert!(store.lock().unwrap().headers_seen.is_empty());
    }

    #[tokio::test]
    async fn a_store_refusal_is_not_retried() {
        let (api, _store) = serve_store().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.fs");
        std::fs::write(&path, b"abc").unwrap();
        let mut t = token(MAX_FILE_BYTES);
        t.client_token = "vercel_blob_client_store1_wrong".into();
        let r = BlobClient::new(&api).upload(&t, &path, Arc::new(|_, _| {})).await;
        assert_eq!(r, Err(BlobError::Refused("Access denied".into())));
    }

    #[tokio::test]
    async fn downloads_only_from_the_file_store() {
        let dir = tempfile::tempdir().unwrap();
        let r = BlobClient::new("http://127.0.0.1:1").download("https://evil.example/x.fs", &dir.path().join("x"), None, Arc::new(|_, _| {})).await;
        if !cfg!(feature = "e2e") {
            assert_eq!(r, Err(BlobError::NotAllowed));
            assert!(!dir.path().join("x").exists());
        }
    }
}
