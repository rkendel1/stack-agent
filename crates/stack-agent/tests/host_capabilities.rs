//! Operator-supplied capabilities through the real work loop: the model is scripted, everything
//! else is real (the environment decorator, validation, the child process, evaluation).
//! Nothing here asserts a model's skill: it asserts what the runtime lets become real.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chip_cli::host_capabilities::{HostCapabilities, HostEnvironmentProvider, HostSpec};
use chip_cli::local_environment::LocalEnvironmentProvider;
use chip_cli::software_work::{GoalKind, SoftwareWork, run_software_work_kind};
use chip_core::{Environments, WorkId, WorkLimits, WorkOutcome};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

struct Script(Mutex<VecDeque<String>>);

#[async_trait::async_trait]
impl ModelProvider for Script {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
        let mut replies = self.0.lock().unwrap();
        let reply = if replies.len() > 1 {
            replies.pop_front()
        } else {
            replies.front().cloned()
        }
        .ok_or_else(|| FxError::Provider("script exhausted".into()))?;
        Ok(ModelResponse::new("m", reply, Usage::new(1, 1)))
    }
}

const LIMITS: WorkLimits = WorkLimits {
    max_turns: 8,
    max_executions: 6,
};

fn scratch(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("hostcaps-it-{}-{name}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const FILE: &str = r#"{"schema":"chip.host-capabilities.v1","capabilities":[
  {"id":"notes.add","description":"Append a note to notes.txt","effect":"consequential",
   "inputs":[{"name":"text","description":"the note","required":true}],
   "binding":{"type":"exec","argv":["sh","-c","printf '%s\\n' \"$1\" >> notes.txt","sh","${text}"]}},
  {"id":"notes.fail","description":"Always fails","effect":"consequential",
   "binding":{"type":"exec","argv":["sh","-c","exit 3"]}},
  {"id":"notes.count","description":"Count notes","effect":"observation",
   "binding":{"type":"exec","argv":["sh","-c","wc -l < notes.txt"]}}]}"#;

async fn act(dir: &Path, replies: &[&str], require: &[&str]) -> SoftwareWork {
    let file = dir.join("caps.json");
    std::fs::write(&file, FILE).unwrap();
    let spec = HostSpec {
        file: Some(file),
        keep_builtin: false,
        require_success: require.iter().map(|s| s.to_string()).collect(),
        ..HostSpec::default()
    };
    let loaded = HostCapabilities::load(&spec, dir, &|_| None).unwrap();
    let inner = Arc::new(LocalEnvironmentProvider::prepare(dir, false).await.unwrap());
    let provider = HostEnvironmentProvider::new(inner, Arc::new(loaded));
    let environment = Environments::new(Arc::new(provider))
        .acquire(WorkId::new("act"))
        .await
        .unwrap();
    let model = Arc::new(Script(Mutex::new(
        replies.iter().map(|s| s.to_string()).collect(),
    )));
    run_software_work_kind(
        GoalKind::Act,
        WorkId::new("act"),
        model,
        "scripted".into(),
        environment.environment(),
        "Record the note 'hello'.",
        LIMITS,
        GoalKind::Act.policy(),
        None,
    )
    .await
}

fn request(capability: &str, inputs: &str) -> String {
    format!(r#"{{"decision":"request_capability","capability":"{capability}","inputs":{inputs}}}"#)
}
const DONE: &str = r#"{"decision":"complete","summary":"recorded the note"}"#;

fn completed(w: &SoftwareWork) -> bool {
    matches!(w.report.outcome, WorkOutcome::Completed { .. })
}

#[tokio::test]
async fn a_requested_action_really_runs_and_is_verified_from_its_own_observation() {
    let dir = scratch("ok");
    let w = act(
        &dir,
        &[&request("notes.add", r#"{"text":"hello"}"#), DONE],
        &["notes.add"],
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(dir.join("notes.txt")).unwrap(),
        "hello\n"
    );
    assert!(completed(&w) && w.verified, "{:?}", w.report.outcome);
}

#[tokio::test]
async fn a_completion_claim_without_the_required_action_is_not_accepted() {
    let dir = scratch("claim");
    let w = act(&dir, &[DONE], &["notes.add"]).await;
    assert!(!w.verified);
    assert!(!completed(&w), "{:?}", w.report.outcome);
    assert!(!dir.join("notes.txt").exists());
}

#[tokio::test]
async fn a_failed_action_does_not_satisfy_the_requirement() {
    let dir = scratch("fail");
    let w = act(&dir, &[&request("notes.fail", "{}"), DONE], &["notes.fail"]).await;
    assert!(!w.verified);
    assert!(!completed(&w), "{:?}", w.report.outcome);
}

#[tokio::test]
async fn invented_inputs_do_not_become_a_run() {
    let dir = scratch("invented");
    // `notes.count` declares no inputs: even an empty object is refused (Invariant 3).
    let w = act(&dir, &[&request("notes.count", "{}"), DONE], &[]).await;
    assert!(!w.verified);
    assert!(!dir.join("notes.txt").exists());
    // An undeclared input name on a capability that declares others (its own directory: a project
    // belongs to one work at a time).
    let dir = scratch("invented-extra");
    let w = act(
        &dir,
        &[
            &request("notes.add", r#"{"text":"a","argv":["rm","-rf","."]}"#),
            DONE,
        ],
        &[],
    )
    .await;
    assert!(!w.verified);
    assert!(!dir.join("notes.txt").exists());
}

#[tokio::test]
async fn the_model_cannot_name_a_builtin_capability_that_was_replaced() {
    let dir = scratch("builtin");
    std::fs::write(dir.join("secret.txt"), "x").unwrap();
    let w = act(
        &dir,
        &[&request("project.read", r#"{"path":"secret.txt"}"#), DONE],
        &[],
    )
    .await;
    assert!(!w.verified);
}

// ---- AppPort: a mock host, so discovery, authorization and calls are really exchanged over HTTP ----

use chip_cli::host_capabilities::AppPortSource;
use std::io::{Read, Write};
use std::net::TcpListener;

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

struct Mock {
    origin: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    fn calls(&self) -> Vec<Seen> {
        self.requests()
            .into_iter()
            .filter(|s| s.method == "POST")
            .collect()
    }
}

/// `answer(seen)` returns `Some((status, body))`, or `None` to drop the connection unanswered.
fn mock(answer: impl Fn(&Seen) -> Option<(u16, String)> + Send + 'static) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let (head_end, length) = loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break (0, 0);
                }
                raw.extend_from_slice(&buf[..n]);
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..end]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (end + 4, length);
                }
            };
            if head_end == 0 {
                continue;
            }
            while raw.len() < head_end + length {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
            }
            let text = String::from_utf8_lossy(&raw).to_string();
            let mut lines = text[..head_end].lines();
            let mut first = lines.next().unwrap_or("").split(' ');
            let request = Seen {
                method: first.next().unwrap_or("").to_string(),
                path: first.next().unwrap_or("").to_string(),
                headers: lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
                    .collect(),
                body: text[head_end..].to_string(),
            };
            log.lock().unwrap().push(request.clone());
            if let Some((status, body)) = answer(&request) {
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        }
    });
    Mock { origin, seen }
}

const MANIFEST: &str = r#"{"capabilities":[
 {"name":"tasks.create","latestVersion":1,"kind":"request","effect":"write","idempotency":"supported",
  "description":"Create a task","authorization":["tasks:write"],
  "inputSchema":{"type":"object","properties":{"title":{"type":"string","maxLength":80}},"required":["title"],"additionalProperties":false}},
 {"name":"tasks.list","latestVersion":1,"kind":"request","effect":"read","description":"List tasks",
  "authorization":["tasks:read"],"inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
 {"name":"admin.purge","latestVersion":1,"kind":"request","effect":"destructive","description":"Delete everything",
  "authorization":["admin"],"inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
 {"name":"notes.secret","latestVersion":1,"kind":"request","effect":"read","description":"Not authorized",
  "authorization":["tasks:read"],"inputSchema":{"type":"object","properties":{},"additionalProperties":false}}]}"#;

fn ok_envelope() -> String {
    r#"{"protocol":"appport/1","type":"response","requestId":"r","ok":true,"output":{"id":"t1"}}"#
        .to_string()
}

fn single_app_answer(call: impl Fn(&Seen) -> Option<(u16, String)> + Send + 'static) -> Mock {
    mock(move |s| match (s.method.as_str(), s.path.as_str()) {
        ("GET", "/.well-known/appport") => Some((
            200,
            r#"{"protocol":"appport/1","manifest":"/manifest","endpoint":"/appport"}"#.into(),
        )),
        ("GET", "/manifest") => Some((200, MANIFEST.to_string())),
        ("POST", "/appport") => call(s),
        _ => Some((404, "{}".into())),
    })
}

fn appport_source(origin: &str) -> AppPortSource {
    AppPortSource {
        origin: origin.to_string(),
        token_env: Some("MOCK_TOKEN".into()),
        grant: vec!["tasks:read".into(), "tasks:write".into()],
        authorize: vec![
            "tasks.create".into(),
            "tasks.list".into(),
            "admin.purge".into(),
        ],
        ..AppPortSource::default()
    }
}

fn load_appport(source: AppPortSource) -> Result<HostCapabilities, String> {
    let spec = HostSpec {
        appport: vec![source],
        keep_builtin: false,
        require_success: vec!["tasks.create".into()],
        ..HostSpec::default()
    };
    HostCapabilities::load(&spec, Path::new("/tmp"), &|name| {
        (name == "MOCK_TOKEN").then(|| "s3cret-token".to_string())
    })
}

async fn act_with(dir: &Path, loaded: HostCapabilities, replies: &[&str]) -> SoftwareWork {
    let inner = Arc::new(LocalEnvironmentProvider::prepare(dir, false).await.unwrap());
    let provider = HostEnvironmentProvider::new(inner, Arc::new(loaded));
    let environment = Environments::new(Arc::new(provider))
        .acquire(WorkId::new("act"))
        .await
        .unwrap();
    let model = Arc::new(Script(Mutex::new(
        replies.iter().map(|s| s.to_string()).collect(),
    )));
    run_software_work_kind(
        GoalKind::Act,
        WorkId::new("act"),
        model,
        "scripted".into(),
        environment.environment(),
        "Create the task 'ship it'.",
        LIMITS,
        GoalKind::Act.policy(),
        None,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_authorizes_nothing_and_withholds_with_a_reason() {
    let host = single_app_answer(|_| Some((200, ok_envelope())));
    let origin = host.origin.clone();
    let loaded = tokio::task::spawn_blocking(move || load_appport(appport_source(&origin)))
        .await
        .unwrap()
        .unwrap();
    let report = loaded.report();
    let offered: Vec<&str> = report["offered"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(offered, vec!["tasks.create", "tasks.list"]);
    let withheld = report["withheld"].to_string();
    assert!(
        withheld.contains("admin.purge") && withheld.contains("did not grant"),
        "{withheld}"
    );
    assert!(
        withheld.contains("notes.secret") && withheld.contains("not authorized"),
        "{withheld}"
    );
    // No credential on discovery requests, and none anywhere in the report.
    for seen in host.requests() {
        assert_eq!(seen.header("authorization"), None, "{seen:?}");
    }
    assert!(!report.to_string().contains("s3cret-token"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_authorized_call_carries_the_token_and_exactly_the_declared_input() {
    let host = single_app_answer(|_| Some((200, ok_envelope())));
    let origin = host.origin.clone();
    let loaded = tokio::task::spawn_blocking(move || load_appport(appport_source(&origin)))
        .await
        .unwrap()
        .unwrap();
    let dir = scratch("call");
    let w = act_with(
        &dir,
        loaded,
        &[&request("tasks.create", r#"{"title":"ship it"}"#), DONE],
    )
    .await;
    assert!(completed(&w) && w.verified, "{:?}", w.report.outcome);
    let calls = host.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].header("authorization"),
        Some("Bearer s3cret-token")
    );
    let body: serde_json::Value = serde_json::from_str(&calls[0].body).unwrap();
    assert_eq!(body["input"], serde_json::json!({"title":"ship it"}));
    assert_eq!(body["capability"]["name"], "tasks.create");
    assert_eq!(body["protocol"], "appport/1");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_connection_is_unknown_and_is_not_retried_or_counted_as_success() {
    let host = single_app_answer(|_| None);
    let origin = host.origin.clone();
    let loaded = tokio::task::spawn_blocking(move || load_appport(appport_source(&origin)))
        .await
        .unwrap()
        .unwrap();
    let dir = scratch("drop");
    let w = act_with(
        &dir,
        loaded,
        &[&request("tasks.create", r#"{"title":"ship it"}"#), DONE],
    )
    .await;
    assert!(!w.verified, "{:?}", w.report.outcome);
    assert_eq!(host.calls().len(), 1, "a lost answer must not be retried");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_from_the_host_is_a_failure_not_a_success() {
    let host = single_app_answer(|_| {
        Some((
            403,
            r#"{"protocol":"appport/1","type":"response","requestId":"r","ok":false,"error":{"code":"FORBIDDEN","message":"no"}}"#.into(),
        ))
    });
    let origin = host.origin.clone();
    let loaded = tokio::task::spawn_blocking(move || load_appport(appport_source(&origin)))
        .await
        .unwrap()
        .unwrap();
    let dir = scratch("refused");
    let w = act_with(
        &dir,
        loaded,
        &[&request("tasks.create", r#"{"title":"ship it"}"#), DONE],
    )
    .await;
    assert!(!w.verified && !completed(&w), "{:?}", w.report.outcome);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_capability_that_was_withheld_cannot_be_called() {
    let host = single_app_answer(|_| Some((200, ok_envelope())));
    let origin = host.origin.clone();
    let loaded = tokio::task::spawn_blocking(move || load_appport(appport_source(&origin)))
        .await
        .unwrap()
        .unwrap();
    let dir = scratch("withheld");
    let w = act_with(&dir, loaded, &[&request("admin.purge", "{}"), DONE]).await;
    assert!(!w.verified);
    assert!(host.calls().is_empty(), "{:?}", host.calls());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_host_needs_a_reviewed_manifest_and_sends_the_granted_scopes() {
    let host = mock(|s| {
        match (s.method.as_str(), s.path.as_str()) {
        ("GET", "/.well-known/appport") => Some((
            200,
            r#"{"protocol":"AppPort/host-discovery/1","applications":[{"id":"app1","status":"running","capabilities":["tasks.create@1","tasks.list@1"]}]}"#.into(),
        )),
        ("POST", p) if p.starts_with("/apps/app1/capabilities/") => Some((200, ok_envelope())),
        _ => Some((404, "{}".into())),
    }
    });
    let mut source = appport_source(&host.origin);
    source.application = Some("app1".into());
    let refused = load_appport(source.clone()).err().unwrap_or_default();
    assert!(refused.contains("manifest"), "{refused}");

    let dir = scratch("desktop");
    let manifest = dir.join("manifest.json");
    std::fs::write(&manifest, MANIFEST).unwrap();
    source.manifest = Some(manifest.display().to_string());
    let loaded = tokio::task::spawn_blocking(move || load_appport(source))
        .await
        .unwrap()
        .unwrap();
    // The manifest declares capabilities the host does not serve: reported, not offered.
    assert!(
        loaded.report()["withheld"]
            .to_string()
            .contains("does not serve")
    );
    let w = act_with(
        &dir,
        loaded,
        &[&request("tasks.create", r#"{"title":"ship it"}"#), DONE],
    )
    .await;
    assert!(completed(&w) && w.verified, "{:?}", w.report.outcome);
    let calls = host.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].path.contains("tasks.create"), "{}", calls[0].path);
    assert_eq!(calls[0].header("x-appport-scopes"), Some("tasks:write"));
}
