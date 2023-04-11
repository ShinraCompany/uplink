use flume::{bounded, Receiver, RecvError, Sender, TrySendError};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use tokio::select;
use tokio::time::{self, interval, Sleep};

use std::{collections::HashMap, fmt::Debug, pin::Pin, sync::Arc, time::Duration};

mod metrics;
pub mod stream;
mod utils;

use crate::base::{default_timeout, Action, ActionResponse, Package, Payload, DEFAULT_TIMEOUT};
pub use metrics::StreamMetrics;
pub use stream::Stream;
use stream::Streams;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Receiver error {0}")]
    Recv(#[from] RecvError),
    #[error("Action receiver busy or down")]
    UnresponsiveReceiver,
    #[error("No route for action {0}")]
    NoRoute(String),
    #[error("Action timedout")]
    ActionTimeout,
    #[error("Another action is currently being processed")]
    Busy,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ActionRoute {
    pub name: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

impl From<&ActionRoute> for ActionRoute {
    fn from(value: &ActionRoute) -> Self {
        value.clone()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct StreamConfig {
    pub topic: String,
    pub buf_size: usize,
    #[serde(default = "default_timeout")]
    /// Duration(in seconds) that bridge collector waits from
    /// receiving first element, before the stream gets flushed.
    pub flush_period: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct StreamMetricsConfig {
    pub enabled: bool,
    pub topic: String,
    pub blacklist: Vec<String>,
    pub timeout: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    pub project_id: String,
    pub device_id: String,
    pub streams: HashMap<String, StreamConfig>,
    pub action_status: StreamConfig,
    pub stream_metrics: StreamMetricsConfig,
    #[serde(default)]
    pub action_redirections: HashMap<String, String>,
    #[serde(default)]
    pub ignore_actions_if_no_clients: bool,
}

#[derive(Debug)]
pub enum Event {
    /// App name and handle for brige to send actions to the app
    RegisterActionRoute(String, ActionRouter),
    /// Data sent by the app
    Data(Payload),
    /// Sometime apps can choose to directly send action response instead
    /// sending in `Payload` form
    ActionResponse(ActionResponse),
}

pub struct Bridge {
    /// All configuration
    config: Arc<Config>,
    /// Tx handle to give to apps
    bridge_tx: Sender<Event>,
    /// Rx to receive events from apps
    bridge_rx: Receiver<Event>,
    /// Handle to send batched data to serialzer
    package_tx: Sender<Box<dyn Package>>,
    /// Handle to send stream metrics to monitor
    metrics_tx: Sender<StreamMetrics>,
    /// Actions incoming from backend
    actions_rx: Receiver<Action>,
    /// Action responses going to backend
    action_status: Stream<ActionResponse>,
    /// Apps registered with the bridge
    /// NOTE: Sometimes action_routes could overlap, the latest route
    /// to be registered will be used in such a circumstance.
    action_routes: HashMap<String, ActionRouter>,
    /// Action redirections
    action_redirections: HashMap<String, String>,
    /// Current action that is being processed
    current_action: Option<CurrentAction>,
}

impl Bridge {
    pub fn new(
        config: Config,
        package_tx: Sender<Box<dyn Package>>,
        metrics_tx: Sender<StreamMetrics>,
        actions_rx: Receiver<Action>,
        action_status: Stream<ActionResponse>,
    ) -> Bridge {
        let (bridge_tx, bridge_rx) = bounded(10);
        let action_redirections = config.action_redirections.clone();
        let config = Arc::new(config);

        Bridge {
            action_status,
            bridge_tx,
            bridge_rx,
            package_tx,
            metrics_tx,
            config,
            actions_rx,
            action_routes: HashMap::with_capacity(10),
            action_redirections,
            current_action: None,
        }
    }

    pub fn tx(&mut self) -> BridgeTx {
        BridgeTx { events_tx: self.bridge_tx.clone() }
    }

    fn clear_current_action(&mut self) {
        self.current_action = None;
    }

    pub async fn start(&mut self) -> Result<(), Error> {
        let mut metrics_timeout = interval(Duration::from_secs(self.config.stream_metrics.timeout));
        let mut streams =
            Streams::new(self.config.clone(), self.package_tx.clone(), self.metrics_tx.clone())
                .await;
        let mut end = Box::pin(time::sleep(Duration::from_secs(u64::MAX)));

        loop {
            select! {
                action = self.actions_rx.recv_async() => {
                    let action = action?;
                    let action_id = action.action_id.clone();
                    // Reactlabs setup processes logs generated by uplink
                    info!("Received action: {:?}", action);

                    if let Some(current_action) = &self.current_action {
                        warn!("Another action is currently occupying uplink; action_id = {}", current_action.id);
                        self.forward_action_error(action, Error::Busy).await;
                        continue
                    }

                    // NOTE: Don't do any blocking operations here
                    // TODO: Remove blocking here. Audit all blocking functions here
                    let error = match self.try_route_action(action.clone()) {
                        Ok(_) => {
                            let response = ActionResponse::progress(&action_id, "Received", 0);
                            self.forward_action_response(response).await;
                            continue;
                        }
                        Err(e) => e,
                    };
                    // Ignore sending failure status to backend. This makes
                    // backend retry action.
                    //
                    // TODO: Do we need this? Shouldn't backend have an easy way to
                    // retry failed actions in bulk?
                    if self.config.ignore_actions_if_no_clients {
                        error!("No clients connected, ignoring action = {:?}", action_id);
                        self.current_action = None;
                        continue;
                    }

                    error!("Failed to route action to app. Error = {:?}", error);
                    self.forward_action_error(action, error).await;
                }
                event = self.bridge_rx.recv_async() => {
                    let event = event?;
                    match event {
                        Event::RegisterActionRoute(name, tx) => {
                            self.action_routes.insert(name, tx);
                        }
                        Event::Data(v) => {
                            streams.forward(v).await;
                        }
                        Event::ActionResponse(response) => {
                            self.forward_action_response(response).await;
                        }
                    }
                }
                _ = &mut self.current_action.as_mut().map(|a| &mut a.timeout).unwrap_or(&mut end) => {
                    let action = self.current_action.take().unwrap();
                    error!("Timeout waiting for action response. Action ID = {}", action.id);
                    self.forward_action_error(action.action, Error::ActionTimeout).await;
                }
                // Flush streams that timeout
                Some(timedout_stream) = streams.stream_timeouts.next(), if streams.stream_timeouts.has_pending() => {
                    debug!("Flushing stream = {}", timedout_stream);
                    if let Err(e) = streams.flush_stream(&timedout_stream).await {
                        error!("Failed to flush stream = {}. Error = {}", timedout_stream, e);
                    }
                }
                // Flush all metrics when timed out
                _ = metrics_timeout.tick() => {
                    if let Err(e) = streams.check_and_flush_metrics() {
                        debug!("Failed to flush stream metrics. Error = {}", e);
                    }
                }
            }
        }
    }

    /// Handle received actions
    fn try_route_action(&mut self, action: Action) -> Result<(), Error> {
        match self.action_routes.get(&action.name) {
            Some(app_tx) => {
                let duration =
                    app_tx.try_send(action.clone()).map_err(|_| Error::UnresponsiveReceiver)?;
                self.current_action = Some(CurrentAction::new(action, duration));

                Ok(())
            }
            None => Err(Error::NoRoute(action.name)),
        }
    }

    async fn forward_action_response(&mut self, response: ActionResponse) {
        let inflight_action = match &mut self.current_action {
            Some(v) => v,
            None => {
                error!("Action timed out already/not present, ignoring response: {:?}", response);
                return;
            }
        };

        if *inflight_action.id != response.action_id {
            error!("response id({}) != active action({})", response.action_id, inflight_action.id);
            return;
        }

        info!("Action response = {:?}", response);
        if let Err(e) = self.action_status.fill(response.clone()).await {
            error!("Failed to fill. Error = {:?}", e);
        }

        if response.is_completed() || response.is_failed() {
            self.clear_current_action();
            return;
        }
        inflight_action.timeout = Box::pin(time::sleep(
            self.action_routes
                .get(&inflight_action.action.name)
                .map(|a| a.duration)
                .unwrap_or(Duration::from_secs(DEFAULT_TIMEOUT)),
        ));

        // Forward actions included in the config to the appropriate forward route, when
        // they have reached 100% progress but haven't been marked as "Completed"/"Finished".
        if response.is_done() {
            let fwd_name = match self.action_redirections.get(&inflight_action.action.name) {
                Some(n) => n,
                None => {
                    // NOTE: send success reponse for actions that don't have redirections configured
                    warn!("Action redirection for {} not configured", inflight_action.action.name);
                    let response = ActionResponse::success(&inflight_action.id);
                    if let Err(e) = self.action_status.fill(response).await {
                        error!("Failed to send status. Error = {:?}", e);
                    }

                    self.clear_current_action();
                    return;
                }
            };

            if let Some(action) = response.done_response {
                inflight_action.action = action;
            }

            let mut fwd_action = inflight_action.action.clone();
            fwd_action.name = fwd_name.to_owned();

            if let Err(e) = self.try_route_action(fwd_action.clone()) {
                error!("Failed to route action to app. Error = {:?}", e);
                self.forward_action_error(fwd_action, e).await;
            }
        }
    }

    async fn forward_action_error(&mut self, action: Action, error: Error) {
        let status = ActionResponse::failure(&action.action_id, error.to_string());

        if let Err(e) = self.action_status.fill(status).await {
            error!("Failed to send status. Error = {:?}", e);
        }

        self.clear_current_action();
    }
}

struct CurrentAction {
    pub id: String,
    pub action: Action,
    pub timeout: Pin<Box<Sleep>>,
}

impl CurrentAction {
    pub fn new(action: Action, duration: Duration) -> CurrentAction {
        CurrentAction {
            id: action.action_id.clone(),
            action,
            timeout: Box::pin(time::sleep(duration)),
        }
    }
}

#[derive(Debug)]
pub struct ActionRouter {
    actions_tx: Sender<Action>,
    duration: Duration,
}

impl ActionRouter {
    #[allow(clippy::result_large_err)]
    pub fn try_send(&self, action: Action) -> Result<Duration, TrySendError<Action>> {
        self.actions_tx.try_send(action)?;

        Ok(self.duration)
    }
}

#[derive(Debug, Clone)]
pub struct BridgeTx {
    // Handle for apps to send events to bridge
    pub events_tx: Sender<Event>,
}

impl BridgeTx {
    pub async fn register_action_route(&self, route: ActionRoute) -> Receiver<Action> {
        let (actions_tx, actions_rx) = bounded(1);
        let duration = Duration::from_secs(route.timeout);
        let action_router = ActionRouter { actions_tx, duration };
        let event = Event::RegisterActionRoute(route.name, action_router);

        // Bridge should always be up and hence unwrap is ok
        self.events_tx.send_async(event).await.unwrap();
        actions_rx
    }

    pub async fn register_action_routes<R: Into<ActionRoute>, V: IntoIterator<Item = R>>(
        &self,
        routes: V,
    ) -> Option<Receiver<Action>> {
        let routes: Vec<ActionRoute> = routes.into_iter().map(|n| n.into()).collect();
        if routes.is_empty() {
            return None;
        }

        let (actions_tx, actions_rx) = bounded(1);

        for route in routes {
            let duration = Duration::from_secs(route.timeout);
            let action_router = ActionRouter { actions_tx: actions_tx.clone(), duration };
            let event = Event::RegisterActionRoute(route.name, action_router);
            // Bridge should always be up and hence unwrap is ok
            self.events_tx.send_async(event).await.unwrap();
        }

        Some(actions_rx)
    }

    pub async fn send_payload(&self, payload: Payload) {
        let event = Event::Data(payload);
        self.events_tx.send_async(event).await.unwrap()
    }

    pub fn send_payload_sync(&self, payload: Payload) {
        let event = Event::Data(payload);
        self.events_tx.send(event).unwrap()
    }

    pub async fn send_action_response(&self, response: ActionResponse) {
        let event = Event::ActionResponse(response);
        self.events_tx.send_async(event).await.unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use flume::{bounded, Receiver, Sender};
    use tokio::{runtime::Runtime, select};

    use super::{stream::Stream, ActionRoute, Bridge, BridgeTx, Config, StreamMetricsConfig};
    use crate::base::{Action, ActionResponse, Package};

    fn default_config() -> Config {
        Config {
            stream_metrics: StreamMetricsConfig {
                enabled: false,
                timeout: 10,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn start_bridge(config: Config) -> (BridgeTx, Sender<Action>, Receiver<Box<dyn Package>>) {
        let (package_tx, package_rx) = bounded(10);
        let (metrics_tx, _) = bounded(10);
        let (actions_tx, actions_rx) = bounded(10);
        let action_status = Stream::dynamic_with_size("", "", "", 1, package_tx.clone());

        let mut bridge = Bridge::new(config, package_tx, metrics_tx, actions_rx, action_status);
        let bridge_tx = bridge.tx();

        std::thread::spawn(move || {
            let rt = Runtime::new().unwrap();
            rt.block_on(async { bridge.start().await.unwrap() });
        });

        (bridge_tx, actions_tx, package_rx)
    }

    fn recv_response(package_rx: &Receiver<Box<dyn Package>>) -> ActionResponse {
        let status = package_rx.recv().unwrap().serialize().unwrap();
        let status: Vec<ActionResponse> = serde_json::from_slice(&status).unwrap();
        status[0].clone()
    }

    #[tokio::test]
    async fn timeout_on_diff_routes() {
        let config = default_config();
        let (bridge_tx, actions_tx, package_rx) = start_bridge(config);
        let route_1 = ActionRoute { name: "route_1".to_string(), timeout: 10 };
        let route_1_rx = bridge_tx.register_action_route(route_1).await;

        let route_2 = ActionRoute { name: "route_2".to_string(), timeout: 30 };
        let route_2_rx = bridge_tx.register_action_route(route_2).await;

        std::thread::spawn(move || {
            let rt = Runtime::new().unwrap();
            rt.block_on(async {
                loop {
                    select! {
                        action = route_1_rx.recv_async() => {
                            let action = action.unwrap();
                            assert_eq!(action.action_id, "1".to_owned());
                        }

                        action = route_2_rx.recv_async() => {
                            let action = action.unwrap();
                            assert_eq!(action.action_id, "2".to_owned());
                        }
                    }
                }
            });
        });

        std::thread::sleep(Duration::from_secs(1));

        let action_1 = Action {
            action_id: "1".to_string(),
            kind: "test".to_string(),
            name: "route_1".to_string(),
            payload: "test".to_string(),
        };
        actions_tx.send(action_1).unwrap();

        let status = recv_response(&package_rx);
        assert_eq!(status.state, "Received".to_owned());
        let start = status.timestamp;

        let status = recv_response(&package_rx);
        // verify response is timeout failure
        assert!(status.is_failed());
        assert_eq!(status.action_id, "1".to_owned());
        assert_eq!(status.errors, ["Action timedout"]);
        let elapsed = status.timestamp - start;
        // verify timeout in 10s
        assert_eq!(elapsed / 1000, 10);

        let action_2 = Action {
            action_id: "2".to_string(),
            kind: "test".to_string(),
            name: "route_2".to_string(),
            payload: "test".to_string(),
        };
        actions_tx.send(action_2).unwrap();

        let status = recv_response(&package_rx);
        assert_eq!(status.state, "Received".to_owned());
        let start = status.timestamp;

        let status = recv_response(&package_rx);
        // verify response is timeout failure
        assert!(status.is_failed());
        assert_eq!(status.action_id, "2".to_owned());
        assert_eq!(status.errors, ["Action timedout"]);
        let elapsed = status.timestamp - start;
        // verify timeout in 30s
        assert_eq!(elapsed / 1000, 30);
    }

    #[tokio::test]
    async fn recv_action_while_current_action_exists() {
        let config = default_config();
        let (bridge_tx, actions_tx, package_rx) = start_bridge(config);

        let test_route = ActionRoute { name: "test".to_string(), timeout: 30 };
        let action_rx = bridge_tx.register_action_route(test_route).await;

        std::thread::spawn(move || loop {
            let action = action_rx.recv().unwrap();
            assert_eq!(action.action_id, "1".to_owned());
        });

        std::thread::sleep(Duration::from_secs(1));

        let action_1 = Action {
            action_id: "1".to_string(),
            kind: "test".to_string(),
            name: "test".to_string(),
            payload: "test".to_string(),
        };
        actions_tx.send(action_1).unwrap();

        let status = recv_response(&package_rx);
        assert_eq!(status.action_id, "1".to_owned());
        assert_eq!(status.state, "Received".to_owned());

        let action_2 = Action {
            action_id: "2".to_string(),
            kind: "test".to_string(),
            name: "test".to_string(),
            payload: "test".to_string(),
        };
        actions_tx.send(action_2).unwrap();

        let status = recv_response(&package_rx);
        // verify response is uplink occupied failure
        assert!(status.is_failed());
        assert_eq!(status.action_id, "2".to_owned());
        assert_eq!(status.errors, ["Another action is currently being processed"]);
    }

    #[tokio::test]
    async fn complete_response_on_no_redirection() {
        let config = default_config();
        let (bridge_tx, actions_tx, package_rx) = start_bridge(config);

        let test_route = ActionRoute { name: "test".to_string(), timeout: 30 };
        let action_rx = bridge_tx.register_action_route(test_route).await;

        std::thread::spawn(move || loop {
            let action = action_rx.recv().unwrap();
            assert_eq!(action.action_id, "1".to_owned());
            std::thread::sleep(Duration::from_secs(1));
            let response = ActionResponse::progress("1", "Tested", 100);
            Runtime::new().unwrap().block_on(bridge_tx.send_action_response(response));
        });

        std::thread::sleep(Duration::from_secs(1));

        let action = Action {
            action_id: "1".to_string(),
            kind: "test".to_string(),
            name: "test".to_string(),
            payload: "test".to_string(),
        };
        actions_tx.send(action).unwrap();

        let status = recv_response(&package_rx);
        assert_eq!(status.state, "Received".to_owned());

        let status = recv_response(&package_rx);
        assert!(status.is_done());
        assert_eq!(status.state, "Tested");

        let status = recv_response(&package_rx);
        assert!(status.is_completed());
    }

    #[tokio::test]
    async fn no_complete_response_between_redirection() {
        let mut config = default_config();
        config.action_redirections.insert("test".to_string(), "redirect".to_string());
        let (bridge_tx, actions_tx, package_rx) = start_bridge(config);
        let bridge_tx_clone = bridge_tx.clone();

        std::thread::spawn(move || loop {
            let rt = Runtime::new().unwrap();
            let test_route = ActionRoute { name: "test".to_string(), timeout: 30 };
            let action_rx = rt.block_on(bridge_tx.register_action_route(test_route));
            let action = action_rx.recv().unwrap();
            assert_eq!(action.action_id, "1".to_owned());
            std::thread::sleep(Duration::from_secs(1));
            let response = ActionResponse::progress("1", "Tested", 100);
            rt.block_on(bridge_tx.send_action_response(response));
        });

        std::thread::spawn(move || loop {
            let rt = Runtime::new().unwrap();
            let test_route = ActionRoute { name: "redirect".to_string(), timeout: 30 };
            let action_rx = rt.block_on(bridge_tx_clone.register_action_route(test_route));
            let action = action_rx.recv().unwrap();
            assert_eq!(action.action_id, "1".to_owned());
            let response = ActionResponse::progress("1", "Redirected", 0);
            rt.block_on(bridge_tx_clone.send_action_response(response));
            std::thread::sleep(Duration::from_secs(1));
            let response = ActionResponse::success("1");
            rt.block_on(bridge_tx_clone.send_action_response(response));
        });

        std::thread::sleep(Duration::from_secs(1));

        let action = Action {
            action_id: "1".to_string(),
            kind: "test".to_string(),
            name: "test".to_string(),
            payload: "test".to_string(),
        };
        actions_tx.send(action).unwrap();

        let status = recv_response(&package_rx);
        assert_eq!(status.state, "Received".to_owned());

        let status = recv_response(&package_rx);
        assert!(status.is_done());
        assert_eq!(status.state, "Tested");

        let status = recv_response(&package_rx);
        assert!(!status.is_completed());
        assert_eq!(status.state, "Redirected");

        let status = recv_response(&package_rx);
        assert!(status.is_completed());
    }
}
