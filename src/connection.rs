#![warn(missing_docs)]

use crate::code::{Code, ParseCode};
use crate::error::EslError;
use crate::esl::EslConnectionType;
use crate::event::Event;
use crate::io::EslCodec;
use crate::{EventHandler, EventManager};
use futures::SinkExt;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;
use tokio::io::WriteHalf;
use tokio::net::TcpStream;
use tokio::sync::{
    oneshot::{channel, Receiver, Sender},
    Mutex,
};
use tokio::task::JoinHandle;

use tokio_stream::StreamExt;
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{trace, warn};

struct EventAwait(String, String);

/// contains Esl connection with freeswitch
#[derive(Clone, Debug)]
pub struct EslConnection {
    password: String,
    commands: Arc<Mutex<VecDeque<Sender<Event>>>>,
    command_mutex: Arc<Mutex<()>>,
    transport_tx: Arc<Mutex<FramedWrite<WriteHalf<TcpStream>, EslCodec>>>,
    background_jobs: Arc<Mutex<HashMap<String, Sender<Event>>>>,
    wait_for_events: Arc<Mutex<HashMap<String, Sender<Event>>>>,
    connected: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    pub(crate) call_uuid: Option<String>,
    connection_info: Option<HashMap<String, Value>>,
    event_manager: Arc<Mutex<EventManager>>,
}

impl EslConnection {
    /// Returns one of the session parameters as a string
    pub fn get_info_string(&self, key: &str) -> Option<String> {
        let value = self.connection_info.as_ref()?.get(key)?.clone();
        serde_json::from_value(value).ok()?
    }

    /// Returns one of the session parameters as any deserializable type
    pub fn get_info<V: DeserializeOwned>(&self, key: &str) -> Option<V> {
        let value = self.connection_info.as_ref()?.get(key)?.clone();
        serde_json::from_value(value).ok()?
    }
    /// Bind a callback event
    pub async fn bind_event(&self, event: String, handler: EventHandler) {
        let mut manager = self.event_manager.lock().await;
        manager.register_handler(event, handler);
    }

    /// returns call uuid in outbound mode
    pub async fn call_uuid(&self) -> Option<String> {
        self.call_uuid.clone()
    }
    /// disconnects from freeswitch
    pub async fn disconnect(self) -> Result<(), EslError> {
        self.send_recv(b"exit").await?;
        self.connected.store(false, Ordering::Relaxed);
        self.failed.store(true, Ordering::Relaxed);
        Ok(())
    }
    /// returns status of esl connection
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    // send raw meesage without freeswitch response
    pub async fn send(&self, item: &[u8]) -> Result<(), EslError> {
        if self.failed.load(Ordering::Relaxed) {
            return Err(EslError::ConnectionClosed);
        }

        let mut transport = self.transport_tx.lock().await;
        transport.send(item).await
    }
    /// sends raw message to freeswitch and receives reply
    pub async fn send_recv(&self, item: &[u8]) -> Result<Event, EslError> {
        let connection = self.clone();
        let item = item.to_vec();
        join_esl_task(tokio::spawn(async move {
            connection.send_recv_inner(&item).await
        }))
        .await
    }

    async fn send_recv_inner(&self, item: &[u8]) -> Result<Event, EslError> {
        let (tx, rx) = channel();
        let _guard = self.command_mutex.lock().await;
        self.commands.lock().await.push_back(tx);
        if let Err(err) = self.send(item).await {
            let _ = self.commands.lock().await.pop_back();
            self.fail_connection().await;
            return Err(err);
        }
        drop(_guard);
        self.recv_event(rx).await
    }

    /// Sends raw message to freeswitch and receives reply with a timeout.
    ///
    /// If this times out after bytes may have been written, the FIFO command
    /// response stream can no longer be trusted. The connection is marked as
    /// failed and all pending waiters are released.
    pub async fn send_recv_timeout(
        &self,
        item: &[u8],
        timeout: Duration,
    ) -> Result<Event, EslError> {
        let connection = self.clone();
        let item = item.to_vec();
        join_esl_task(tokio::spawn(async move {
            connection.send_recv_timeout_inner(&item, timeout).await
        }))
        .await
    }

    async fn send_recv_timeout_inner(
        &self,
        item: &[u8],
        timeout: Duration,
    ) -> Result<Event, EslError> {
        match tokio::time::timeout(timeout, self.send_recv_inner(item)).await {
            Ok(result) => result,
            Err(_) => {
                self.fail_connection().await;
                Err(EslError::Timeout(timeout))
            }
        }
    }

    async fn fail_connection(&self) {
        self.connected.store(false, Ordering::Relaxed);
        self.failed.store(true, Ordering::Relaxed);
        clear_waiters(&self.commands, &self.background_jobs, &self.wait_for_events).await;
    }

    async fn recv_event(&self, rx: Receiver<Event>) -> Result<Event, EslError> {
        match rx.await {
            Ok(event) => Ok(event),
            Err(err) if self.failed.load(Ordering::Relaxed) => {
                trace!("ESL waiter closed after connection failure: {err}");
                Err(EslError::ConnectionClosed)
            }
            Err(err) => Err(err.into()),
        }
    }

    pub async fn wait_for_event(&self, waiter: String, event: String) {
        let (tx, rx) = channel();

        let mut hash = event.clone();
        hash.push_str(&waiter);

        println!("hash inserted {}", hash.clone());

        self.wait_for_events.lock().await.insert(hash.clone(), tx);

        let resp = rx.await;
    }

    pub(crate) async fn new(
        stream: TcpStream,
        password: impl ToString,
        connection_type: EslConnectionType,
    ) -> Result<Self, EslError> {
        // let sender = Arc::new(sender);
        let commands = Arc::new(Mutex::new(VecDeque::new()));
        let inner_commands = Arc::clone(&commands);
        let command_mutex = Arc::new(Mutex::new(()));
        let background_jobs = Arc::new(Mutex::new(HashMap::new()));
        let wait_for_events = Arc::new(Mutex::new(HashMap::new()));
        let inner_background_jobs = Arc::clone(&background_jobs);
        let inner_wait_for_events_jobs = Arc::clone(&wait_for_events);
        let connected = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let inner_connected = Arc::clone(&connected);
        let inner_failed = Arc::clone(&failed);
        let event_manager = Arc::new(Mutex::new(EventManager::new()));
        let event_manager_inner = event_manager.clone();
        let esl_codec = EslCodec {};
        let (read_half, write_half) = tokio::io::split(stream);
        let mut transport_rx = FramedRead::new(read_half, esl_codec.clone());
        let transport_tx = Arc::new(Mutex::new(FramedWrite::new(write_half, esl_codec.clone())));
        if connection_type == EslConnectionType::Inbound {
            transport_rx.next().await;
        }
        let mut connection = Self {
            password: password.to_string(),
            commands,
            command_mutex,
            background_jobs,
            wait_for_events,
            transport_tx,
            connected,
            failed,
            call_uuid: None,
            connection_info: None,
            event_manager: event_manager,
        };
        tokio::spawn(async move {
            while let Some(event_result) = transport_rx.next().await {
                let event = match event_result {
                    Ok(event) => event,
                    Err(err) => {
                        warn!("ESL transport read error: {err}, shutting down reader task");
                        inner_connected.store(false, Ordering::Relaxed);
                        inner_failed.store(true, Ordering::Relaxed);
                        clear_waiters(
                            &inner_commands,
                            &inner_background_jobs,
                            &inner_wait_for_events_jobs,
                        )
                        .await;
                        break;
                    }
                };

                if let Some(event_type) = event.headers.get("Content-Type") {
                    let Some(event_type) = event_type.as_str() else {
                        warn!("ESL event Content-Type is not a string, discarding event");
                        continue;
                    };

                    match event_type {
                        "text/disconnect-notice" => {
                            trace!("got disconnect notice");
                            inner_connected.store(false, Ordering::Relaxed);
                            inner_failed.store(true, Ordering::Relaxed);
                            clear_waiters(
                                &inner_commands,
                                &inner_background_jobs,
                                &inner_wait_for_events_jobs,
                            )
                            .await;
                            return;
                        }
                        "text/event-json" => {
                            let Some(data) = event.body().clone() else {
                                warn!("ESL event-json missing body, discarding event");
                                continue;
                            };

                            let event_body = match parse_json_body(&data) {
                                Ok(body) => body,
                                Err(err) => {
                                    warn!("Unable to parse ESL event-json body: {err}");
                                    continue;
                                }
                            };
                            let job_uuid = event_body.get("Job-UUID");

                            if event_body.contains_key("Event-Name")
                                && event_body.contains_key("Unique-ID")
                            {
                                let event_name = event_body.get("Event-Name");
                                let name = event_name.unwrap().as_str().unwrap().to_string();
                                let unique_id =
                                    event_body.get("Unique-ID").unwrap().as_str().unwrap();

                                let mut hash = name.clone();
                                hash.push_str(unique_id);

                                {
                                    event_manager_inner
                                        .lock()
                                        .await
                                        .trigger_event(name, &event_body)
                                        .await;
                                }

                                if let Some(tx) =
                                    inner_wait_for_events_jobs.lock().await.remove(&hash)
                                {
                                    if tx.send(event.clone()).is_err() {
                                        trace!("wait_for_event receiver dropped, discarding event");
                                    }
                                }
                            }

                            if let Some(job_uuid) = job_uuid {
                                let job_uuid = job_uuid.as_str().unwrap();
                                if let Some(tx) =
                                    inner_background_jobs.lock().await.remove(job_uuid)
                                {
                                    if tx.send(event.clone()).is_err() {
                                        trace!("background job receiver dropped, discarding event");
                                    }
                                }

                                trace!("continued");
                                continue;
                            }
                            if let Some(application_uuid) = event_body.get("Application-UUID") {
                                let job_uuid = application_uuid.as_str().unwrap();
                                if let Some(event_name) = event_body.get("Event-Name") {
                                    if let Some(event_name) = event_name.as_str() {
                                        if event_name == "CHANNEL_EXECUTE_COMPLETE" {
                                            if let Some(tx) =
                                                inner_background_jobs.lock().await.remove(job_uuid)
                                            {
                                                if tx.send(event).is_err() {
                                                    trace!(
                                                        "background job receiver dropped, discarding event"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            continue;
                        }
                        _ => {
                            trace!("got another event {:?}", event);
                        }
                    }
                }
                if let Some(tx) = inner_commands.lock().await.pop_front() {
                    if tx.send(event).is_err() {
                        trace!("command receiver dropped, discarding response");
                    }
                }
            }

            inner_connected.store(false, Ordering::Relaxed);
            inner_failed.store(true, Ordering::Relaxed);
            clear_waiters(
                &inner_commands,
                &inner_background_jobs,
                &inner_wait_for_events_jobs,
            )
            .await;
            trace!("ESL transport_rx finished or closed, exiting reader task");
        });
        match connection_type {
            EslConnectionType::Inbound => {
                let auth_response = connection.auth().await?;

                connection
                    .subscribe(vec!["BACKGROUND_JOB", "CHANNEL_EXECUTE_COMPLETE"])
                    .await?;
            }
            EslConnectionType::Outbound => {
                let response = connection.send_recv(b"connect").await?;
                trace!("{:?}", response);
                connection.connection_info = Some(response.headers().clone());
                let response = connection
                    .subscribe(vec!["BACKGROUND_JOB", "CHANNEL_EXECUTE_COMPLETE"])
                    .await?;
                trace!("{:?}", response);
                let response = connection.send_recv(b"myevents").await?;
                trace!("{:?}", response);
                let connection_info = connection.connection_info.as_ref().unwrap();

                let channel_unique_id = connection_info
                    .get("Channel-Unique-ID")
                    .unwrap()
                    .as_str()
                    .unwrap();
                connection.call_uuid = Some(channel_unique_id.to_string());
            }
        }
        Ok(connection)
    }

    /// subscribes to given events
    pub async fn subscribe(&self, events: Vec<&str>) -> Result<Event, EslError> {
        let message = format!("event json {}", events.join(" "));
        self.send_recv(message.as_bytes()).await
    }

    pub(crate) async fn auth(&self) -> Result<String, EslError> {
        let auth_response = self
            .send_recv(format!("auth {}", self.password).as_bytes())
            .await?;
        let auth_headers = auth_response.headers();
        let reply_text = auth_headers.get("Reply-Text").ok_or_else(|| {
            EslError::InternalError("Reply-Text in auth request was not found".into())
        })?;
        let reply_text = reply_text.as_str().unwrap();
        let (code, text) = parse_api_response(reply_text)?;
        match code {
            Code::Ok => {
                self.connected.store(true, Ordering::Relaxed);
                Ok(text)
            }
            Code::Err => Err(EslError::AuthFailed),
            Code::Unknown => Err(EslError::InternalError(
                "Got unknown code in auth request".into(),
            )),
        }
    }

    /// For hanging up call in outbound mode
    pub async fn hangup(&self, reason: &str) -> Result<Event, EslError> {
        self.execute("hangup", reason).await
    }

    /// executes application in freeswitch
    pub async fn execute(&self, app_name: &str, app_args: &str) -> Result<Event, EslError> {
        let event_uuid = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = channel();
        self.background_jobs
            .lock()
            .await
            .insert(event_uuid.clone(), tx);
        let call_uuid = self.call_uuid.as_ref().unwrap().clone();
        let command  = format!("sendmsg {}\nexecute-app-name: {}\nexecute-app-arg: {}\ncall-command: execute\nEvent-UUID: {}",call_uuid,app_name,app_args,event_uuid);
        let response = self.send_recv(command.as_bytes()).await?;
        trace!("inside execute {:?}", response);
        let resp = self.recv_event(rx).await?;
        trace!("got response from channel {:?}", resp);
        Ok(resp)
    }

    /// answers call in outbound mode
    pub async fn answer(&self) -> Result<Event, EslError> {
        self.execute("answer", "").await
    }

    /// sends api command to freeswitch
    pub async fn api(&self, command: &str) -> Result<String, EslError> {
        let response = self.send_recv(format!("api {}", command).as_bytes()).await;
        let event = response?;
        self.parse_api_event(event)
    }

    /// Sends api command to freeswitch with a timeout.
    ///
    /// Use this only for commands that are expected to return synchronously
    /// within `timeout`.
    pub async fn api_timeout(&self, command: &str, timeout: Duration) -> Result<String, EslError> {
        let event = self
            .send_recv_timeout(format!("api {}", command).as_bytes(), timeout)
            .await?;
        self.parse_api_event(event)
    }

    fn parse_api_event(&self, event: Event) -> Result<String, EslError> {
        let body = event
            .body
            .ok_or_else(|| EslError::InternalError("Didnt get body in api response".into()))?;

        let (code, text) = parse_api_response(&body)?;
        match code {
            Code::Ok => Ok(text),
            Code::Err => Err(EslError::ApiError(text)),
            Code::Unknown => Ok(body),
        }
    }

    /// sends bgapi commands to freeswitch
    pub async fn bgapi(&self, command: &str) -> Result<String, EslError> {
        trace!("Send bgapi {}", command);
        let job_uuid = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = channel();
        self.background_jobs
            .lock()
            .await
            .insert(job_uuid.clone(), tx);

        self.send_recv(format!("bgapi {}\nJob-UUID: {}", command, job_uuid).as_bytes())
            .await?;

        let resp = self.recv_event(rx).await?;
        let body = resp
            .body()
            .clone()
            .ok_or_else(|| EslError::InternalError("body was not found in event/json".into()))?;

        let body_hashmap = parse_json_body(&body)?;

        let mut hsmp = resp.headers().clone();
        hsmp.extend(body_hashmap);
        let body = hsmp
            .get("_body")
            .ok_or_else(|| EslError::InternalError("body was not found in event/json".into()))?;
        let body = body.as_str().unwrap();
        let (code, text) = parse_api_response(body)?;
        match code {
            Code::Ok => Ok(text),
            Code::Err => Err(EslError::ApiError(text)),
            Code::Unknown => Ok(body.to_string()),
        }
    }

    /// Sends bgapi command to freeswitch with separate protocol and job timeouts.
    ///
    /// `reply_timeout` bounds the immediate command/reply frame. If it elapses,
    /// the connection is marked failed because FIFO command replies can no
    /// longer be trusted. `job_timeout` only bounds the correlated
    /// BACKGROUND_JOB event for the generated Job-UUID; on timeout that job
    /// waiter is removed while the connection remains usable.
    pub async fn bgapi_timeout(
        &self,
        command: &str,
        reply_timeout: Duration,
        job_timeout: Duration,
    ) -> Result<String, EslError> {
        let connection = self.clone();
        let command = command.to_string();
        join_esl_task(tokio::spawn(async move {
            connection
                .bgapi_timeout_inner(&command, reply_timeout, job_timeout)
                .await
        }))
        .await
    }

    async fn bgapi_timeout_inner(
        &self,
        command: &str,
        reply_timeout: Duration,
        job_timeout: Duration,
    ) -> Result<String, EslError> {
        trace!("Send bgapi {}", command);
        let job_uuid = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = channel();
        self.background_jobs
            .lock()
            .await
            .insert(job_uuid.clone(), tx);

        let send_result = self
            .send_recv_timeout_inner(
                format!("bgapi {}\nJob-UUID: {}", command, job_uuid).as_bytes(),
                reply_timeout,
            )
            .await;

        if let Err(err) = send_result {
            self.background_jobs.lock().await.remove(&job_uuid);
            return Err(err);
        }

        let resp = match tokio::time::timeout(job_timeout, rx).await {
            Ok(Ok(event)) => event,
            Ok(Err(err)) if self.failed.load(Ordering::Relaxed) => {
                trace!("ESL background job waiter closed after connection failure: {err}");
                return Err(EslError::ConnectionClosed);
            }
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => {
                self.background_jobs.lock().await.remove(&job_uuid);
                return Err(EslError::Timeout(job_timeout));
            }
        };

        parse_bgapi_response(resp)
    }
}

async fn join_esl_task<T>(handle: JoinHandle<Result<T, EslError>>) -> Result<T, EslError> {
    match handle.await {
        Ok(result) => result,
        Err(err) => Err(EslError::InternalError(format!(
            "ESL worker task failed: {err}"
        ))),
    }
}

async fn clear_waiters(
    commands: &Arc<Mutex<VecDeque<Sender<Event>>>>,
    background_jobs: &Arc<Mutex<HashMap<String, Sender<Event>>>>,
    wait_for_events: &Arc<Mutex<HashMap<String, Sender<Event>>>>,
) {
    commands.lock().await.clear();
    background_jobs.lock().await.clear();
    wait_for_events.lock().await.clear();
}

fn parse_bgapi_response(resp: Event) -> Result<String, EslError> {
    let body = resp
        .body()
        .clone()
        .ok_or_else(|| EslError::InternalError("body was not found in event/json".into()))?;

    let body_hashmap = parse_json_body(&body)?;

    let mut hsmp = resp.headers().clone();
    hsmp.extend(body_hashmap);
    let body = hsmp
        .get("_body")
        .ok_or_else(|| EslError::InternalError("body was not found in event/json".into()))?;
    let body = body.as_str().unwrap();
    let (code, text) = parse_api_response(body)?;
    match code {
        Code::Ok => Ok(text),
        Code::Err => Err(EslError::ApiError(text)),
        Code::Unknown => Ok(body.to_string()),
    }
}
fn parse_api_response(body: &str) -> Result<(Code, String), EslError> {
    let space_index = body
        .find(char::is_whitespace)
        .ok_or_else(|| EslError::InternalError("Unable to find space index".into()))?;
    let code = &body[..space_index];
    let text_start = space_index + 1;
    let body_length = body.len();
    let text = if text_start < body_length {
        body[text_start..(body_length - 1)].to_string()
    } else {
        String::new()
    };
    let code = code.parse_code()?;
    Ok((code, text))
}
fn parse_json_body(body: &str) -> Result<HashMap<String, Value>, EslError> {
    Ok(serde_json::from_str(body)?)
}
