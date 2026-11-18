//! Test harness: starts the real server (real sockets, 2 workers, temp DB) on its
//! own thread and actix system, and helpers shared by the integration tests.
//!
//! Not every test file uses every helper.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use actix_web::dev::ServerHandle;
use reqwest::StatusCode;
use serde_json::Value;

use lss_server::config::Config;
pub use lss_server::config::ReadSource;

pub const DB_FILE: &str = "lss.db";

pub struct TestServer {
    pub base: String,
    pub mode: ReadSource,
    handle: ServerHandle,
    _dir: tempfile::TempDir,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    /// Starts the real server on 127.0.0.1:0 with 2 workers and a fresh temp DB.
    pub fn start(mode: ReadSource) -> TestServer {
        Self::start_with_dir(mode, tempfile::tempdir().unwrap())
    }

    /// Same, but reuses an existing DB directory (for persistence tests).
    pub fn start_with_dir(mode: ReadSource, dir: tempfile::TempDir) -> TestServer {
        let config = Config {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            workers: 2,
            database_path: dir.path().join(DB_FILE),
            log_level: "warn".into(),
            read_source: mode,
        };
        let (tx, rx) = mpsc::channel::<Result<(SocketAddr, ServerHandle), String>>();
        let thread = std::thread::spawn(move || {
            actix_web::rt::System::new().block_on(async move {
                match lss_server::build_server(config) {
                    Ok(running) => {
                        tx.send(Ok((running.addr, running.handle.clone()))).unwrap();
                        running.run().await.unwrap();
                    }
                    Err(e) => tx.send(Err(format!("{e:#}"))).unwrap(),
                }
            });
        });
        let (addr, handle) = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("server thread never reported")
            .unwrap_or_else(|e| panic!("server failed to start: {e}"));
        wait_healthy(addr);
        TestServer {
            base: format!("http://{addr}"),
            mode,
            handle,
            _dir: dir,
            thread: Some(thread),
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self._dir.path().join(DB_FILE)
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Stops the server gracefully and returns the DB directory.
    ///
    /// Drop any `reqwest::Client` first: graceful shutdown waits (up to 5 s) for
    /// open keep-alive connections.
    pub fn stop(mut self) -> tempfile::TempDir {
        // `stop` sends the command immediately; the returned future only waits
        // for completion, which joining the thread below does too.
        drop(self.handle.stop(true));
        if let Some(t) = self.thread.take() {
            t.join().expect("server thread panicked");
        }
        let dir = tempfile::tempdir().unwrap();
        std::mem::replace(&mut self._dir, dir)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            drop(self.handle.stop(false));
            let _ = t.join();
        }
    }
}

/// Blocking `GET /health` loop with a raw socket: `start` is a plain fn and must
/// not need an async runtime.
fn wait_healthy(addr: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
            let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = s.write_all(b"GET /health HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n");
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            if buf.starts_with("HTTP/1.1 200") {
                return;
            }
        }
        assert!(Instant::now() < deadline, "server never became healthy");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs an `async fn(&TestServer)` once per read mode, as `<name>::sqlite` and
/// `<name>::memory`. (Modules and functions live in different namespaces, so
/// the module can share the function's name.)
#[macro_export]
macro_rules! both_modes {
    ($($name:ident),+ $(,)?) => {$(
        mod $name {
            #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
            async fn sqlite() {
                let srv = $crate::common::TestServer::start($crate::common::ReadSource::Sqlite);
                super::$name(&srv).await;
                srv.stop();
            }

            #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
            async fn memory() {
                let srv = $crate::common::TestServer::start($crate::common::ReadSource::Memory);
                super::$name(&srv).await;
                srv.stop();
            }
        }
    )+};
}

// ---- HTTP helpers --------------------------------------------------------------

/// One response, with the checks every test wants done for free.
#[derive(Debug, Clone)]
pub struct Resp {
    pub status: StatusCode,
    pub text: String,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {:?}", self.text))
    }

    /// Asserts status and `{"error": code}`.
    pub fn assert_error(&self, status: u16, code: &str) {
        assert_eq!(self.status.as_u16(), status, "body: {}", self.text);
        assert_eq!(self.json(), serde_json::json!({ "error": code }));
    }
}

/// A client with a large keep-alive pool, so concurrency tests reuse connections.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(256)
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

async fn finish(resp: reqwest::Response) -> Resp {
    let status = resp.status();
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned());
    let text = resp.text().await.unwrap();
    // Spec: every response has Content-Type: application/json.
    assert_eq!(
        ctype.as_deref(),
        Some("application/json"),
        "{status} {text}"
    );
    Resp { status, text }
}

pub async fn get(c: &reqwest::Client, url: &str) -> Resp {
    finish(c.get(url).send().await.unwrap()).await
}

/// POST with a raw body (so tests can send invalid JSON).
pub async fn post_raw(c: &reqwest::Client, url: &str, body: impl Into<reqwest::Body>) -> Resp {
    let resp = c
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    finish(resp).await
}

pub async fn post(c: &reqwest::Client, url: &str, body: &Value) -> Resp {
    post_raw(c, url, serde_json::to_vec(body).unwrap()).await
}

pub fn product(id: &str, name: &str, category: &str, price: i64, stock: i64) -> Value {
    serde_json::json!({"id": id, "name": name, "category": category, "price": price, "stock": stock})
}

pub fn order(user: &str, product: &str) -> Value {
    serde_json::json!({"userId": user, "productId": product})
}

/// Creates a product and asserts 201.
pub async fn create(srv: &TestServer, c: &reqwest::Client, p: &Value) -> Resp {
    let r = post(c, &srv.url("/products"), p).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r
}

pub async fn stock_of(srv: &TestServer, c: &reqwest::Client, id: &str) -> i64 {
    let r = get(c, &srv.url(&format!("/products/{id}"))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json()["stock"].as_i64().unwrap()
}

pub async fn orders_of(srv: &TestServer, c: &reqwest::Client, user: &str) -> Vec<Value> {
    let r = get(c, &srv.url(&format!("/users/{user}/orders"))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let v = r.json();
    assert_eq!(v["userId"], user);
    v["orders"].as_array().unwrap().clone()
}
