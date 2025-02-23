//! Sets up a webserver for WebSocket and HTTP interaction with clients.

use crate::{
    db::Database,
    runner,
    runner::{DynamicNodeInfo, StaticNodeInfo, WsSender},
    settings,
};
use anyhow::{anyhow, Result};
use faststr::FastStr;
use homestar_core::Workflow;
use homestar_wasm::io::Arg;
use http::{
    header::{AUTHORIZATION, CONTENT_TYPE},
    method::Method,
};
use jsonrpsee::{
    self,
    server::{middleware::http::ProxyGetRequestLayer, RandomStringIdProvider, ServerHandle},
};
use libipld::Cid;
use metrics_exporter_prometheus::PrometheusHandle;
use std::{
    iter::once,
    net::{IpAddr, SocketAddr, TcpListener},
    str::FromStr,
    time::Duration,
};
use tokio::runtime::Handle;
#[cfg(feature = "websocket-notify")]
use tokio::sync::broadcast;
use tower_http::{
    cors::{self, CorsLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
};
use tracing::info;

pub(crate) mod listener;
#[cfg(feature = "websocket-notify")]
pub(crate) mod notifier;
mod prom;
mod rpc;

#[cfg(feature = "websocket-notify")]
pub(crate) use notifier::Notifier;
#[cfg(feature = "websocket-notify")]
pub(crate) use rpc::SUBSCRIBE_NETWORK_EVENTS_ENDPOINT;
use rpc::{Context, JsonRpc};

/// Message type for messages sent back from the
/// WebSocket server to the [runner] for example.
///
/// [runner]: crate::Runner
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum Message {
    /// Error attempting to run a [Workflow].
    RunErr(runner::Error),
    /// Run a workflow, given a tuple of name, and [Workflow].
    RunWorkflow((FastStr, Workflow<'static, Arg>)),
    /// Acknowledgement of a [Workflow] run.
    AckWorkflow((Cid, FastStr)),
    /// Message sent to the [Runner] to gather node information from the [EventHandler].
    ///
    /// [Runner]: crate::Runner
    /// [EventHandler]: crate::EventHandler
    GetNodeInfo,
    /// Acknowledgement of a [Message::GetNodeInfo] request, receiving static and dynamic
    /// node information.
    AckNodeInfo((StaticNodeInfo, DynamicNodeInfo)),
}

/// Server fields.
#[cfg(feature = "websocket-notify")]
#[derive(Clone, Debug)]
pub(crate) struct Server {
    /// Address of the server.
    addr: SocketAddr,
    /// Message buffer capacity for the server.
    capacity: usize,
    /// Message sender for broadcasting internal events to clients connected to
    /// to the server.
    evt_notifier: Notifier<notifier::Message>,
    /// Message sender for broadcasting workflow-related events to clients
    /// connected to to the server.
    workflow_msg_notifier: Notifier<notifier::Message>,
    /// Receiver timeout for the server when communicating with the [Runner].
    ///
    /// [Runner]: crate::Runner
    receiver_timeout: Duration,
    /// General timeout for the server.
    webserver_timeout: Duration,
}

/// Server fields.
#[cfg(not(feature = "websocket-notify"))]
#[derive(Clone, Debug)]
pub(crate) struct Server {
    /// Address of the server.
    addr: SocketAddr,
    /// Message buffer capacity for the server.
    capacity: usize,
    /// Receiver timeout for the server when communicating with the [Runner].
    ///
    /// [Runner]: crate::Runner
    receiver_timeout: Duration,
    /// General timeout for the server.
    webserver_timeout: Duration,
}

impl Server {
    /// Setup bounded, MPMC channel for runtime to send and received messages
    /// through the WebSocket connection(s).
    #[cfg(feature = "websocket-notify")]
    fn setup_channel(
        capacity: usize,
    ) -> (
        broadcast::Sender<notifier::Message>,
        broadcast::Receiver<notifier::Message>,
    ) {
        broadcast::channel(capacity)
    }

    /// Set up a new [Server] instance, which acts as both a
    /// WebSocket and HTTP server.
    #[cfg(feature = "websocket-notify")]
    pub(crate) fn new(settings: &settings::Webserver) -> Result<Self> {
        let (evt_sender, _receiver) = Self::setup_channel(settings.websocket_capacity);
        let (msg_sender, _receiver) = Self::setup_channel(settings.websocket_capacity);
        let host = IpAddr::from_str(&settings.host.to_string())?;
        let port_setting = settings.port;
        let addr = if port_available(host, port_setting) {
            SocketAddr::from((host, port_setting))
        } else {
            let port = (port_setting..port_setting + 1000)
                .find(|port| port_available(host, *port))
                .ok_or_else(|| anyhow!("no free TCP ports available"))?;
            SocketAddr::from((host, port))
        };

        Ok(Self {
            addr,
            capacity: settings.websocket_capacity,
            evt_notifier: Notifier::new(evt_sender),
            workflow_msg_notifier: Notifier::new(msg_sender),
            receiver_timeout: settings.websocket_receiver_timeout,
            webserver_timeout: settings.timeout,
        })
    }

    /// Set up a new [Server] instance, which only acts as an HTTP server.
    #[cfg(not(feature = "websocket-notify"))]
    pub(crate) fn new(settings: &settings::Webserver) -> Result<Self> {
        let host = IpAddr::from_str(&settings.host.to_string())?;
        let port_setting = settings.port;
        let addr = if port_available(host, port_setting) {
            SocketAddr::from((host, port_setting))
        } else {
            let port = (port_setting..port_setting + 1000)
                .find(|port| port_available(host, *port))
                .ok_or_else(|| anyhow!("no free TCP ports available"))?;
            SocketAddr::from((host, port))
        };

        Ok(Self {
            addr,
            capacity: settings.websocket_capacity,
            receiver_timeout: settings.websocket_receiver_timeout,
            webserver_timeout: settings.timeout,
        })
    }

    /// Instantiates the [JsonRpc] module, and starts the server.
    #[cfg(feature = "websocket-notify")]
    pub(crate) async fn start(
        &self,
        runner_sender: WsSender,
        metrics_hdl: PrometheusHandle,
        db: impl Database + 'static,
    ) -> Result<ServerHandle> {
        let module = JsonRpc::new(Context::new(
            metrics_hdl,
            self.evt_notifier.clone(),
            self.workflow_msg_notifier.clone(),
            runner_sender,
            db,
            self.receiver_timeout,
        ))
        .await?;

        self.start_inner(module).await
    }

    /// Instantiates the [JsonRpc] module, and starts the server.
    #[cfg(not(feature = "websocket-notify"))]
    pub(crate) async fn start(
        &self,
        runner_sender: WsSender,
        metrics_hdl: PrometheusHandle,
        db: impl Database + 'static,
    ) -> Result<ServerHandle> {
        let module = JsonRpc::new(Context::new(
            metrics_hdl,
            runner_sender,
            db,
            self.receiver_timeout,
        ))
        .await?;
        self.start_inner(module).await
    }

    /// Return the WebSocket event sender for broadcasting messages to connected
    /// clients.
    #[cfg(feature = "websocket-notify")]
    pub(crate) fn evt_notifier(&self) -> Notifier<notifier::Message> {
        self.evt_notifier.clone()
    }

    /// Get WebSocket message sender for broadcasting workflow-related messages
    /// to connected clients.
    #[cfg(feature = "websocket-notify")]
    pub(crate) fn workflow_msg_notifier(&self) -> Notifier<notifier::Message> {
        self.workflow_msg_notifier.clone()
    }

    /// Shared start logic for both WebSocket and HTTP servers.
    async fn start_inner<DB: Database + 'static>(
        &self,
        module: JsonRpc<DB>,
    ) -> Result<ServerHandle> {
        let addr = self.addr;
        info!(
            subject = "webserver.start",
            category = "webserver",
            "webserver listening on {}",
            addr
        );

        let cors = CorsLayer::new()
            // Allow `POST` when accessing the resource
            .allow_methods([Method::GET, Method::POST])
            // Allow requests from any origin
            .allow_origin(cors::Any)
            .allow_headers([CONTENT_TYPE]);

        let middleware = tower::ServiceBuilder::new()
            .layer(ProxyGetRequestLayer::new("/health", rpc::HEALTH_ENDPOINT)?)
            .layer(ProxyGetRequestLayer::new(
                "/metrics",
                rpc::METRICS_ENDPOINT,
            )?)
            .layer(ProxyGetRequestLayer::new("/node", rpc::NODE_INFO_ENDPOINT)?)
            .layer(cors)
            .layer(SetSensitiveRequestHeadersLayer::new(once(AUTHORIZATION)))
            .timeout(self.webserver_timeout);

        let runtime_hdl = Handle::current();

        let server = jsonrpsee::server::Server::builder()
            .custom_tokio_runtime(runtime_hdl.clone())
            .set_http_middleware(middleware)
            .set_id_provider(Box::new(RandomStringIdProvider::new(16)))
            .set_message_buffer_capacity(self.capacity as u32)
            .build(addr)
            .await
            .expect("Webserver to startup");

        let hdl = server.start(module.into_inner());
        runtime_hdl.spawn(hdl.clone().stopped());

        Ok(hdl)
    }
}

fn port_available(host: IpAddr, port: u16) -> bool {
    TcpListener::bind((host.to_string(), port)).is_ok()
}

#[cfg(test)]
mod test {
    use super::*;
    #[cfg(feature = "websocket-notify")]
    use crate::event_handler::notification::ReceiptNotification;
    use crate::{channel::AsyncChannel, settings::Settings, test_utils::db::MemoryDb};
    #[cfg(feature = "websocket-notify")]
    use homestar_core::{
        ipld::DagJson,
        test_utils,
        workflow::{config::Resources, instruction::RunInstruction, prf::UcanPrf, Task},
    };
    #[cfg(feature = "websocket-notify")]
    use jsonrpsee::core::client::{error::Error as ClientError, Subscription, SubscriptionClientT};
    #[cfg(feature = "websocket-notify")]
    use jsonrpsee::types::error::ErrorCode;
    use jsonrpsee::{core::client::ClientT, rpc_params, ws_client::WsClientBuilder};
    #[cfg(feature = "websocket-notify")]
    use notifier::{self, Header};

    async fn metrics_handle(settings: Settings) -> PrometheusHandle {
        #[cfg(feature = "monitoring")]
        let metrics_hdl =
            crate::metrics::start(settings.node.monitoring(), settings.node.network())
                .await
                .unwrap();

        #[cfg(not(feature = "monitoring"))]
        let metrics_hdl = crate::metrics::start(settings.node.network())
            .await
            .unwrap();

        metrics_hdl
    }

    #[homestar_runtime_proc_macro::runner_test]
    fn ws_connect() {
        let TestRunner { runner, settings } = TestRunner::start();
        runner.runtime.block_on(async {
            let server = Server::new(settings.node().network().webserver()).unwrap();
            let db = MemoryDb::setup_connection_pool(settings.node(), None).unwrap();
            let metrics_hdl = metrics_handle(settings).await;
            let (runner_tx, _runner_rx) = AsyncChannel::oneshot();
            server.start(runner_tx, metrics_hdl, db).await.unwrap();

            let ws_url = format!("ws://{}", server.addr);
            let http_url = format!("http://{}", server.addr);

            tokio_tungstenite::connect_async(ws_url.clone())
                .await
                .unwrap();

            let client = WsClientBuilder::default().build(ws_url).await.unwrap();
            let ws_resp: serde_json::Value = client
                .request(rpc::HEALTH_ENDPOINT, rpc_params![])
                .await
                .unwrap();
            assert_eq!(ws_resp, serde_json::json!({"healthy": true }));
            let http_resp = reqwest::get(format!("{}/health", http_url)).await.unwrap();
            assert_eq!(http_resp.status(), 200);
            let http_resp = http_resp.json::<serde_json::Value>().await.unwrap();
            assert_eq!(http_resp, serde_json::json!({"healthy": true }));
        });

        unsafe { metrics::clear_recorder() }
    }

    #[cfg(feature = "monitoring")]
    #[homestar_runtime_proc_macro::runner_test]
    async fn ws_metrics_no_prefix() {
        let TestRunner { runner, settings } = TestRunner::start();
        runner.runtime.block_on(async {
            let server = Server::new(settings.node().network().webserver()).unwrap();
            let db = MemoryDb::setup_connection_pool(settings.node(), None).unwrap();
            let metrics_hdl = metrics_handle(settings).await;
            let (runner_tx, _runner_rx) = AsyncChannel::oneshot();
            server.start(runner_tx, metrics_hdl, db).await.unwrap();

            let ws_url = format!("ws://{}", server.addr);

            // wait for interval to pass
            std::thread::sleep(Duration::from_millis(150));

            let client = WsClientBuilder::default().build(ws_url).await.unwrap();
            let ws_resp1: serde_json::Value = client
                .request(rpc::METRICS_ENDPOINT, rpc_params![])
                .await
                .unwrap();

            let len = if let serde_json::Value::Array(array) = &ws_resp1["metrics"] {
                array.len()
            } else {
                panic!("expected array");
            };

            assert!(len > 0);

            unsafe { metrics::clear_recorder() }
        });
    }

    #[cfg(feature = "websocket-notify")]
    #[homestar_runtime_proc_macro::runner_test]
    async fn ws_subscribe_unsubscribe_network_events() {
        let TestRunner { runner, settings } = TestRunner::start();
        runner.runtime.block_on(async {
            let server = Server::new(settings.node().network().webserver()).unwrap();
            let db = MemoryDb::setup_connection_pool(settings.node(), None).unwrap();
            let metrics_hdl = metrics_handle(settings).await;
            let (runner_tx, _runner_rx) = AsyncChannel::oneshot();
            server.start(runner_tx, metrics_hdl, db).await.unwrap();

            let ws_url = format!("ws://{}", server.addr);

            let client1 = WsClientBuilder::default().build(ws_url).await.unwrap();
            let mut sub: Subscription<Vec<u8>> = client1
                .subscribe(
                    rpc::SUBSCRIBE_NETWORK_EVENTS_ENDPOINT,
                    rpc_params![],
                    rpc::UNSUBSCRIBE_NETWORK_EVENTS_ENDPOINT,
                )
                .await
                .unwrap();

            // send any bytes through (Vec<u8>)
            let (invocation_receipt, runtime_receipt) = crate::test_utils::receipt::receipts();
            let receipt =
                ReceiptNotification::with(invocation_receipt, runtime_receipt.cid(), None);
            server
                .evt_notifier
                .notify(notifier::Message::new(
                    Header::new(
                        notifier::SubscriptionTyp::EventSub(
                            rpc::SUBSCRIBE_NETWORK_EVENTS_ENDPOINT.to_string(),
                        ),
                        None,
                    ),
                    receipt.to_json().unwrap(),
                ))
                .unwrap();

            // send an unknown msg: this should be dropped
            server
                .evt_notifier
                .notify(notifier::Message::new(
                    Header::new(
                        notifier::SubscriptionTyp::EventSub("test".to_string()),
                        None,
                    ),
                    vec![],
                ))
                .unwrap();

            server
                .evt_notifier
                .notify(notifier::Message::new(
                    Header::new(
                        notifier::SubscriptionTyp::EventSub(
                            rpc::SUBSCRIBE_NETWORK_EVENTS_ENDPOINT.to_string(),
                        ),
                        None,
                    ),
                    receipt.to_json().unwrap(),
                ))
                .unwrap();

            let msg1 = sub.next().await.unwrap().unwrap();
            let returned1: ReceiptNotification = DagJson::from_json(&msg1).unwrap();
            assert_eq!(returned1, receipt);

            let msg2 = sub.next().await.unwrap().unwrap();
            let _returned1: ReceiptNotification = DagJson::from_json(&msg2).unwrap();

            assert!(sub.unsubscribe().await.is_ok());

            unsafe { metrics::clear_recorder() }
        });
    }

    #[cfg(feature = "websocket-notify")]
    #[homestar_runtime_proc_macro::runner_test]
    async fn ws_subscribe_workflow_incorrect_params() {
        let TestRunner { runner, settings } = TestRunner::start();
        runner.runtime.block_on(async {
            let server = Server::new(settings.node().network().webserver()).unwrap();
            let db = MemoryDb::setup_connection_pool(settings.node(), None).unwrap();
            let metrics_hdl = metrics_handle(settings).await;
            let (runner_tx, _runner_rx) = AsyncChannel::oneshot();
            server.start(runner_tx, metrics_hdl, db).await.unwrap();

            let ws_url = format!("ws://{}", server.addr);

            let client = WsClientBuilder::default().build(ws_url).await.unwrap();
            let sub: Result<Subscription<Vec<u8>>, ClientError> = client
                .subscribe(
                    rpc::SUBSCRIBE_RUN_WORKFLOW_ENDPOINT,
                    rpc_params![],
                    rpc::UNSUBSCRIBE_RUN_WORKFLOW_ENDPOINT,
                )
                .await;

            assert!(sub.is_err());

            if let Err(ClientError::Call(err)) = sub {
                let check = ErrorCode::InvalidParams;
                assert_eq!(err.code(), check.code());
            } else {
                panic!("expected same error code");
            }

            unsafe { metrics::clear_recorder() }
        });
    }

    #[cfg(feature = "websocket-notify")]
    #[homestar_runtime_proc_macro::runner_test]
    async fn ws_subscribe_workflow_runner_timeout() {
        let TestRunner { runner, settings } = TestRunner::start();
        runner.runtime.block_on(async {
            let server = Server::new(settings.node().network().webserver()).unwrap();
            let db = MemoryDb::setup_connection_pool(settings.node(), None).unwrap();
            let metrics_hdl = metrics_handle(settings).await;
            let (runner_tx, _runner_rx) = AsyncChannel::oneshot();

            server.start(runner_tx, metrics_hdl, db).await.unwrap();

            let ws_url = format!("ws://{}", server.addr);

            let config = Resources::default();
            let instruction1 = test_utils::workflow::instruction::<Arg>();
            let (instruction2, _) = test_utils::workflow::wasm_instruction_with_nonce::<Arg>();

            let task1 = Task::new(
                RunInstruction::Expanded(instruction1),
                config.clone().into(),
                UcanPrf::default(),
            );
            let task2 = Task::new(
                RunInstruction::Expanded(instruction2),
                config.into(),
                UcanPrf::default(),
            );

            let workflow = Workflow::new(vec![task1.clone(), task2.clone()]);
            let run_str = format!(
                r#"{{"name": "test","workflow": {}}}"#,
                workflow.to_json_string().unwrap()
            );

            let run: serde_json::Value = serde_json::from_str(&run_str).unwrap();
            let client = WsClientBuilder::default().build(ws_url).await.unwrap();
            let sub: Result<Subscription<Vec<u8>>, ClientError> = client
                .subscribe(
                    rpc::SUBSCRIBE_RUN_WORKFLOW_ENDPOINT,
                    rpc_params![run],
                    rpc::UNSUBSCRIBE_RUN_WORKFLOW_ENDPOINT,
                )
                .await;

            assert!(sub.is_err());

            // Assure error is not on parse of params, but due to runner
            // timeout (as runner is not available).
            if let Err(ClientError::Call(err)) = sub {
                let check = ErrorCode::ServerIsBusy;
                assert_eq!(err.code(), check.code());
            } else {
                panic!("expected same error code");
            }

            unsafe { metrics::clear_recorder() }
        });
    }
}
