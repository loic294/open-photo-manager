//! The operations client: one [`IsmBatch`] becomes one ISM operation, its items come back as ISM
//! settles them. Nothing here touches a file. Refusals are final (never retried, never done
//! locally); only an unanswered or 5xx request is retried, always with the same idempotency key
//! and body, which ISM replays.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use opm_core::dto::IsmInfoDto;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::wire::writes_allowed;
use super::{IsmClient, IsmService, REQUEST_TIMEOUT};
use crate::db::now_secs;
use crate::fsops::{Delegate, FsOpError, IsmAction, IsmBatch, IsmFile, IsmItem};

/// Waits between attempts of a request that got no usable answer.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];
/// How many times in a row following an operation may fail before OPM gives up on it.
const MAX_FOLLOW_FAILURES: u32 = 60;
/// Capabilities that mean a request may carry `allow_partial`.
const PARTIAL_CAPABILITIES: [&str; 3] = ["partial_submit", "allow_partial", "partial_operations"];
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: Issue,
}

#[derive(Debug, Clone, Deserialize)]
struct Issue {
    code: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct PreviewItem {
    index: usize,
    #[serde(default)]
    blockers: Vec<Issue>,
}

#[derive(Debug, Deserialize)]
struct Preview {
    #[serde(default)]
    items: Vec<PreviewItem>,
    #[serde(default)]
    blockers: Vec<Issue>,
}

#[derive(Debug, Deserialize)]
struct Accepted {
    operation_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CancelByKey {
    CancelledBeforeSubmission,
    Operation { operation: Operation },
}

#[derive(Debug, Clone, Deserialize)]
struct Operation {
    #[serde(default)]
    operation_id: String,
    status: String,
    #[serde(default)]
    items: Vec<OperationItem>,
}

impl Operation {
    fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "partial" | "failed" | "cancelled" | "recovery_required"
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
struct OperationItem {
    index: usize,
    status: String,
    #[serde(default)]
    error: Option<Issue>,
    #[serde(default)]
    result: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct ItemResult {
    #[serde(default)]
    files: Vec<IsmFile>,
}

/// A request that failed: `Refused` is ISM's final answer, `Missing` the same for a 404 or 405
/// (an unknown resource, or a route an older server does not have), `Unknown` means the request
/// may or may not have been applied.
enum Failure {
    Refused(FsOpError),
    Missing(FsOpError),
    Unknown,
}

impl Failure {
    fn into_error(self) -> FsOpError {
        match self {
            Self::Refused(err) | Self::Missing(err) => err,
            Self::Unknown => unreachable_error(),
        }
    }
}

fn unreachable_error() -> FsOpError {
    FsOpError::delegated("ism_unreachable", "The sync manager could not be reached")
}

fn issue_error(issue: &Issue) -> FsOpError {
    FsOpError::delegated(&issue.code, issue.message.clone())
}

/// Sends what ISM settles to the file operations, each index once.
struct Reporter {
    items: mpsc::UnboundedSender<IsmItem>,
    sent: Vec<bool>,
}

impl Reporter {
    fn report(&mut self, index: usize, outcome: Result<Vec<IsmFile>, FsOpError>) {
        if let Some(sent) = self.sent.get_mut(index)
            && !std::mem::replace(sent, true)
        {
            match &outcome {
                Ok(files) => tracing::debug!(item = index, files = files.len(), "ISM item done"),
                Err(err) => {
                    tracing::warn!(item = index, code = err.code(), "ISM item failed");
                    tracing::debug!(item = index, error = %err, "ISM item failure detail");
                }
            }
            let _ = self.items.send(IsmItem { index, outcome });
        }
    }

    fn fail_rest(&mut self, err: &FsOpError) {
        for index in 0..self.sent.len() {
            self.report(index, Err(err.clone()));
        }
    }
}

/// An authenticated client bound to the instance the connection was made with.
struct Session {
    client: IsmClient,
    instance_id: String,
}

impl Session {
    fn url(&self, path: &str) -> Result<Url, FsOpError> {
        self.client.base_url.join(path).map_err(FsOpError::internal)
    }

    fn authorization(&self) -> HeaderValue {
        self.client.authorization.clone()
    }

    /// One request, answered with JSON.
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, Failure> {
        let url = self.url(path).map_err(Failure::Refused)?;
        let mut request = self
            .client
            .http
            .request(method, url)
            .header(AUTHORIZATION, self.authorization())
            .timeout(REQUEST_TIMEOUT);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|_| Failure::Unknown)?;
        let status = response.status();
        let bytes = read_capped(response).await.ok_or(Failure::Unknown)?;
        if status.is_success() {
            return serde_json::from_slice(&bytes).map_err(|_| {
                Failure::Refused(FsOpError::delegated(
                    "ism_error",
                    "The sync manager sent an unreadable answer",
                ))
            });
        }
        let issue = serde_json::from_slice::<ErrorBody>(&bytes)
            .ok()
            .map(|body| body.error);
        if status.is_server_error()
            && issue
                .as_ref()
                .is_none_or(|issue| issue.code != "root_unavailable")
        {
            return Err(Failure::Unknown);
        }
        let err = match issue {
            Some(issue) => issue_error(&issue),
            None => FsOpError::delegated(
                "ism_error",
                format!(
                    "The sync manager refused the request (HTTP {})",
                    status.as_u16()
                ),
            ),
        };
        if matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            return Err(Failure::Missing(err));
        }
        Err(Failure::Refused(err))
    }

    /// [`Self::call`], retried while the answer is unknown. Only for idempotent requests.
    async fn call_retried(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, Failure> {
        let mut delays = RETRY_DELAYS.iter();
        loop {
            match self.call(method.clone(), path, body).await {
                Err(Failure::Unknown) => match delays.next() {
                    Some(delay) => tokio::time::sleep(*delay).await,
                    None => return Err(Failure::Unknown),
                },
                other => return other,
            }
        }
    }

    async fn operation(&self, id: &str) -> Result<Operation, Failure> {
        let value = self
            .call(Method::GET, &format!("api/v1/operations/{id}"), None)
            .await?;
        parse(value).map_err(Failure::Refused)
    }

    /// The operation submitted with `key`, from servers that can look it up; `None` when the
    /// server cannot (an older one) or knows no such operation.
    async fn lookup(&self, key: &str) -> Result<Option<Operation>, Failure> {
        let mut path = format!("api/v1/instances/{}/operations?", self.instance_id);
        path.push_str(
            &url::form_urlencoded::Serializer::new(String::new())
                .append_pair("idempotency_key", key)
                .finish(),
        );
        let value = match self.call_retried(Method::GET, &path, None).await {
            Ok(value) => value,
            Err(Failure::Missing(_)) => return Ok(None),
            Err(err) => return Err(err),
        };
        // An operation, `{"operation": …}`, or a list of matches.
        let value = match value {
            Value::Array(items) => match items.into_iter().next() {
                Some(item) => item,
                None => return Ok(None),
            },
            Value::Object(mut fields) if fields.contains_key("operation") => {
                fields.remove("operation").unwrap_or(Value::Null)
            }
            Value::Null => return Ok(None),
            other => other,
        };
        if value.is_null() {
            return Ok(None);
        }
        let operation: Operation = parse(value).map_err(Failure::Refused)?;
        Ok((!operation.operation_id.is_empty()).then_some(operation))
    }

    async fn cancel(&self, id: &str) -> Result<(), Failure> {
        self.call(
            Method::POST,
            &format!("api/v1/operations/{id}/cancel"),
            None,
        )
        .await
        .map(drop)
    }

    async fn events(&self, id: &str, last_id: Option<&str>) -> Option<reqwest::Response> {
        let url = self.url(&format!("api/v1/operations/{id}/events")).ok()?;
        let mut request = self
            .client
            .http
            .get(url)
            .header(AUTHORIZATION, self.authorization())
            .header("accept", "text/event-stream");
        if let Some(last_id) = last_id {
            request = request.header("last-event-id", last_id);
        }
        let response = request.send().await.ok()?;
        (response.status() == StatusCode::OK).then_some(response)
    }
}

fn parse<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, FsOpError> {
    serde_json::from_value(value).map_err(|_| {
        FsOpError::delegated("ism_error", "The sync manager sent an unreadable answer")
    })
}

async fn read_capped(mut response: reqwest::Response) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some(bytes)
}

/// One server-sent event.
#[derive(Debug, Default, PartialEq, Eq)]
struct Event {
    id: Option<String>,
    name: String,
    data: String,
}

/// Splits a server-sent event stream into events.
#[derive(Default)]
struct EventParser {
    buffer: String,
}

impl EventParser {
    /// Adds bytes and returns the events they complete; `None` when an event is too large.
    fn push(&mut self, chunk: &[u8]) -> Option<Vec<Event>> {
        self.buffer
            .push_str(&String::from_utf8_lossy(chunk).replace("\r\n", "\n"));
        let mut events = Vec::new();
        while let Some(end) = self.buffer.find("\n\n") {
            let block: String = self.buffer.drain(..end + 2).collect();
            let mut event = Event::default();
            let mut data = Vec::new();
            for line in block.lines() {
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "id" => event.id = Some(value.to_owned()),
                    "event" => value.clone_into(&mut event.name),
                    "data" => data.push(value),
                    _ => {}
                }
            }
            if data.is_empty() && event.name.is_empty() {
                if event.id.is_some() {
                    events.push(event);
                }
                continue;
            }
            event.data = data.join("\n");
            events.push(event);
        }
        (self.buffer.len() <= MAX_EVENT_BYTES).then_some(events)
    }
}

/// Follows an operation until it ends, reporting each item as soon as it is settled.
struct Follower<'a> {
    session: &'a Session,
    operation_id: String,
    /// Index in the submitted request → index in the batch.
    mapping: &'a [usize],
    reporter: &'a mut Reporter,
    cancel: CancellationToken,
    cancel_sent: bool,
}

impl Follower<'_> {
    fn settle_item(&mut self, item: &OperationItem, final_status: Option<&str>) {
        let Some(&index) = self.mapping.get(item.index) else {
            return;
        };
        let outcome = match item.status.as_str() {
            "done" => Ok(item
                .result
                .clone()
                .and_then(|result| serde_json::from_value::<ItemResult>(result).ok())
                .unwrap_or_default()
                .files),
            "failed" | "recovery_required" => Err(item.error.as_ref().map_or_else(
                || FsOpError::delegated(&item.status, "The sync manager could not make the change"),
                issue_error,
            )),
            _ => match final_status {
                None => return,
                Some("cancelled") => Err(FsOpError::Cancelled),
                Some("recovery_required") => Err(FsOpError::delegated(
                    "recovery_required",
                    "The sync manager stopped and needs recovery",
                )),
                Some(_) => Err(item.error.as_ref().map_or_else(
                    || FsOpError::delegated("ism_error", "The sync manager skipped the change"),
                    issue_error,
                )),
            },
        };
        self.reporter.report(index, outcome);
    }

    /// Applies a full operation; true once it has ended.
    fn settle(&mut self, operation: &Operation) -> bool {
        let terminal = operation.is_terminal();
        let status = terminal.then_some(operation.status.as_str());
        for item in &operation.items {
            self.settle_item(item, status);
        }
        terminal
    }

    async fn forward_cancel(&mut self) {
        if self.cancel.is_cancelled() && !self.cancel_sent {
            match self.session.cancel(&self.operation_id).await {
                Ok(()) => self.cancel_sent = true,
                Err(Failure::Refused(err) | Failure::Missing(err)) => {
                    // ISM answered (for instance: the operation already ended): stop asking.
                    tracing::debug!(code = err.code(), "ISM did not cancel the operation");
                    self.cancel_sent = true;
                }
                Err(Failure::Unknown) => {}
            }
        }
    }

    /// Reads the event stream until it ends; true once the operation has ended.
    async fn read_stream(
        &mut self,
        response: reqwest::Response,
        last_id: &mut Option<String>,
    ) -> bool {
        let mut stream = response.bytes_stream();
        let mut parser = EventParser::default();
        loop {
            let chunk = if self.cancel_sent {
                stream.next().await
            } else {
                tokio::select! {
                    chunk = stream.next() => chunk,
                    () = self.cancel.cancelled() => {
                        self.forward_cancel().await;
                        if self.cancel_sent {
                            continue;
                        }
                        return false;
                    }
                }
            };
            let Some(Ok(chunk)) = chunk else {
                return false;
            };
            let Some(events) = parser.push(&chunk) else {
                return false;
            };
            for event in events {
                if let Some(id) = event.id.clone() {
                    *last_id = Some(id);
                }
                match event.name.as_str() {
                    "item" => {
                        if let Ok(item) = serde_json::from_str::<OperationItem>(&event.data) {
                            self.settle_item(&item, None);
                        }
                    }
                    "progress" | "finished" => {
                        if let Ok(operation) = serde_json::from_str::<Operation>(&event.data)
                            && self.settle(&operation)
                        {
                            return true;
                        }
                    }
                    // The cursor is older than what ISM kept: read the operation again.
                    "resync" => return false,
                    _ => {}
                }
            }
        }
    }

    async fn run(mut self) -> Result<(), FsOpError> {
        let mut last_id: Option<String> = None;
        let mut failures = 0;
        loop {
            self.forward_cancel().await;
            if let Some(response) = self
                .session
                .events(&self.operation_id, last_id.as_deref())
                .await
            {
                failures = 0;
                if self.read_stream(response, &mut last_id).await {
                    return Ok(());
                }
            } else {
                failures += 1;
            }
            match self.session.operation(&self.operation_id).await {
                Ok(operation) => {
                    if self.settle(&operation) {
                        return Ok(());
                    }
                }
                Err(Failure::Refused(err) | Failure::Missing(err)) => return Err(err),
                Err(Failure::Unknown) => failures += 1,
            }
            if failures >= MAX_FOLLOW_FAILURES {
                return Err(unreachable_error());
            }
            let pause = Duration::from_secs(u64::from(failures.clamp(1, 5)));
            if self.cancel_sent || self.cancel.is_cancelled() {
                tokio::time::sleep(pause).await;
            } else {
                tokio::select! {
                    () = tokio::time::sleep(pause) => {}
                    () = self.cancel.cancelled() => {}
                }
            }
        }
    }
}

/// Why `info` says an action cannot run now; checked before anything is sent.
fn preflight(info: &IsmInfoDto, action: &IsmAction) -> Option<FsOpError> {
    if let Some(grant) = action.grant()
        && !info.actions.iter().any(|granted| granted == grant)
    {
        return Some(FsOpError::delegated(
            "forbidden",
            format!("The ISM token does not allow {grant}"),
        ));
    }
    for root_id in action.roots() {
        let Some(root) = info.roots.iter().find(|root| root.root_id == root_id) else {
            return Some(FsOpError::delegated(
                "root_unavailable",
                "The ISM root bound to this volume is not offered any more",
            ));
        };
        if !root.available {
            return Some(FsOpError::delegated(
                "root_unavailable",
                "The ISM root is not available",
            ));
        }
        if !writes_allowed(root) {
            let why = root
                .direct_writes_reason
                .as_deref()
                .unwrap_or(&root.direct_writes);
            return Some(FsOpError::delegated(
                "direct_writes_unavailable",
                format!("ISM does not change files of this root now ({why})"),
            ));
        }
    }
    None
}

impl IsmService {
    async fn session(&self) -> Result<(Session, IsmInfoDto), FsOpError> {
        let (client, stored, info) = self.connected().await.map_err(|err| {
            let code = match err.code() {
                "ism_not_connected" => "ism_not_connected",
                "ism_unavailable" => "ism_unreachable",
                _ => "ism_error",
            };
            FsOpError::delegated(code, err.message().to_owned())
        })?;
        Ok((
            Session {
                client,
                instance_id: stored.instance_id,
            },
            info,
        ))
    }

    async fn record(&self, key: &str, instance_id: &str, request: &Value) -> Result<(), FsOpError> {
        let now = now_secs();
        sqlx::query(
            "INSERT INTO ism_operations
                 (idempotency_key, instance_id, request_json, status, created_at, updated_at)
             VALUES (?, ?, ?, 'submitting', ?, ?)",
        )
        .bind(key)
        .bind(instance_id)
        .bind(request.to_string())
        .bind(now)
        .bind(now)
        .execute(self.db.write())
        .await
        .map_err(FsOpError::internal)?;
        Ok(())
    }

    async fn note(&self, key: &str, operation_id: Option<&str>, status: &str) {
        match status {
            "submitted" | "finished" => {
                tracing::info!(key, operation_id, status, "ISM operation state");
            }
            _ => tracing::warn!(key, operation_id, status, "ISM operation state"),
        }
        let result = sqlx::query(
            "UPDATE ism_operations SET operation_id = COALESCE(?, operation_id), status = ?,
                 updated_at = ? WHERE idempotency_key = ?",
        )
        .bind(operation_id)
        .bind(status)
        .bind(now_secs())
        .bind(key)
        .execute(self.db.write())
        .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "recording an ISM operation failed");
        }
    }

    async fn run_batch(
        &self,
        batch: &IsmBatch,
        cancel: &CancellationToken,
        reporter: &mut Reporter,
    ) -> Result<(), FsOpError> {
        if cancel.is_cancelled() {
            return Err(FsOpError::Cancelled);
        }
        let (session, info) = self.session().await?;
        let mut mapping = Vec::new();
        for (index, action) in batch.actions.iter().enumerate() {
            match preflight(&info, action) {
                Some(err) => reporter.report(index, Err(err)),
                None => mapping.push(index),
            }
        }
        if mapping.is_empty() {
            return Ok(());
        }

        // Servers that run what they can report blocked actions as failed items themselves;
        // older ones refuse a whole request for one blocker, so a preview finds them first.
        let partial = info
            .capabilities
            .iter()
            .any(|capability| PARTIAL_CAPABILITIES.contains(&capability.as_str()));
        let mapping = if partial {
            mapping
        } else {
            match self.preview(&session, batch, mapping, reporter).await? {
                Some(mapping) => mapping,
                None => return Ok(()),
            }
        };
        if cancel.is_cancelled() {
            return Err(FsOpError::Cancelled);
        }

        let key = format!("{}:{:016x}", batch.key_prefix, rand::random::<u64>());
        let body = request(&key, batch, &mapping, partial);
        self.record(&key, &session.instance_id, &body).await?;
        let submit = format!("api/v1/instances/{}/operations", session.instance_id);
        let operation_id = match session
            .call_retried(Method::POST, &submit, Some(&body))
            .await
        {
            Ok(value) => parse::<Accepted>(value)?.operation_id,
            Err(Failure::Refused(err) | Failure::Missing(err)) => {
                self.note(&key, None, "refused").await;
                return Err(err);
            }
            Err(Failure::Unknown) => {
                tracing::warn!(key, "ISM submit outcome unknown; looking the operation up");
                // The request may have been applied: look it up by key where the server can,
                // else ask ISM to cancel it by key, which also says whether it exists.
                if let Some(operation) = session.lookup(&key).await.map_err(Failure::into_error)? {
                    self.note(&key, Some(&operation.operation_id), "submitted")
                        .await;
                    return self
                        .follow(
                            &session,
                            &key,
                            operation.operation_id,
                            &mapping,
                            cancel,
                            reporter,
                        )
                        .await;
                }
                let probe = json!({"idempotency_key": key, "expected_request": body});
                let path = format!(
                    "api/v1/instances/{}/operations/cancel-by-key",
                    session.instance_id
                );
                match session
                    .call_retried(Method::POST, &path, Some(&probe))
                    .await
                {
                    Ok(value) => match parse::<CancelByKey>(value)? {
                        CancelByKey::CancelledBeforeSubmission => {
                            self.note(&key, None, "cancelled_before_submission").await;
                            return Err(unreachable_error());
                        }
                        CancelByKey::Operation { operation } => operation.operation_id,
                    },
                    Err(err) => {
                        self.note(&key, None, "unknown").await;
                        return Err(err.into_error());
                    }
                }
            }
        };
        self.note(&key, Some(&operation_id), "submitted").await;
        self.follow(&session, &key, operation_id, &mapping, cancel, reporter)
            .await
    }

    /// Asks ISM which actions it would refuse and reports them; the rest, or `None` when
    /// nothing is left to send.
    async fn preview(
        &self,
        session: &Session,
        batch: &IsmBatch,
        mapping: Vec<usize>,
        reporter: &mut Reporter,
    ) -> Result<Option<Vec<usize>>, FsOpError> {
        let preview_key = format!(
            "{}:preview:{:016x}",
            batch.key_prefix,
            rand::random::<u64>()
        );
        let body = request(&preview_key, batch, &mapping, false);
        let preview: Preview = parse(
            session
                .call_retried(Method::POST, "api/v1/operations/preview", Some(&body))
                .await
                .map_err(Failure::into_error)?,
        )?;
        if let Some(blocker) = preview.blockers.first() {
            return Err(issue_error(blocker));
        }
        let mut blocked = vec![false; mapping.len()];
        for item in &preview.items {
            if let (Some(blocker), Some(&index)) = (item.blockers.first(), mapping.get(item.index))
            {
                blocked[item.index] = true;
                reporter.report(index, Err(issue_error(blocker)));
            }
        }
        let mapping: Vec<usize> = mapping
            .into_iter()
            .zip(blocked)
            .filter_map(|(index, blocked)| (!blocked).then_some(index))
            .collect();
        Ok((!mapping.is_empty()).then_some(mapping))
    }

    /// Follows a submitted operation to its end.
    async fn follow(
        &self,
        session: &Session,
        key: &str,
        operation_id: String,
        mapping: &[usize],
        cancel: &CancellationToken,
        reporter: &mut Reporter,
    ) -> Result<(), FsOpError> {
        tracing::info!(
            operation_id,
            actions = mapping.len(),
            "ISM operation submitted"
        );
        let follower = Follower {
            session,
            operation_id,
            mapping,
            reporter,
            cancel: cancel.clone(),
            cancel_sent: false,
        };
        let outcome = follower.run().await;
        self.note(key, None, if outcome.is_ok() { "finished" } else { "lost" })
            .await;
        outcome
    }
}

/// The operation request for the `mapping` actions of `batch`.
fn request(key: &str, batch: &IsmBatch, mapping: &[usize], partial: bool) -> Value {
    let actions: Vec<&IsmAction> = mapping
        .iter()
        .filter_map(|&index| batch.actions.get(index))
        .collect();
    let mut body = json!({
        "idempotency_key": key,
        "label": batch.label,
        "actions": actions,
        "allow_owner_transfer": false,
    });
    if partial {
        body["allow_partial"] = Value::Bool(true);
    }
    body
}

#[async_trait]
impl Delegate for IsmService {
    async fn run(
        &self,
        batch: IsmBatch,
        cancel: CancellationToken,
        items: mpsc::UnboundedSender<IsmItem>,
    ) {
        let mut reporter = Reporter {
            items,
            sent: vec![false; batch.actions.len()],
        };
        if let Err(err) = self.run_batch(&batch, &cancel, &mut reporter).await {
            tracing::warn!(
                key_prefix = %batch.key_prefix,
                code = err.code(),
                error = %err,
                "ISM batch failed"
            );
            reporter.fail_rest(&err);
        }
        reporter.fail_rest(&FsOpError::delegated(
            "ism_error",
            "The sync manager did not report this change",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_split_across_chunks() {
        let mut parser = EventParser::default();
        assert_eq!(
            parser
                .push(b": keep-alive\n\nid: 3\nevent: item\nda")
                .unwrap(),
            vec![]
        );
        let events = parser
            .push(b"ta: {\"a\":1}\r\n\r\nevent: resync\ndata: {}\n\n")
            .unwrap();
        assert_eq!(
            events,
            vec![
                Event {
                    id: Some("3".into()),
                    name: "item".into(),
                    data: "{\"a\":1}".into()
                },
                Event {
                    id: None,
                    name: "resync".into(),
                    data: "{}".into()
                },
            ]
        );
    }
}
