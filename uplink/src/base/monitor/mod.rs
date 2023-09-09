use std::io;
use std::sync::Arc;
use std::time::Duration;

use flume::{Receiver, RecvError};
use rumqttc::{AsyncClient, ClientError, QoS, Request};
use tokio::select;
use tokio::time::interval;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::StreamExt;

use crate::base::bridge::StreamMetrics;
use crate::collector::ActionsLog;
use crate::Config;

use super::mqtt::MqttMetrics;
use super::serializer::SerializerMetrics;

/// Interface implementing MQTT protocol to communicate with broker
pub struct Monitor {
    /// Uplink config
    config: Arc<Config>,
    /// Client handle
    client: AsyncClient,
    /// Stream metrics receiver
    stream_metrics_rx: Receiver<StreamMetrics>,
    /// Serializer metrics receiver
    serializer_metrics_rx: Receiver<SerializerMetrics>,
    /// Mqtt metrics receiver
    mqtt_metrics_rx: Receiver<MqttMetrics>,
    /// app metrics shared memory
    actions_log: ActionsLog,
}

impl Monitor {
    pub fn new(
        config: Arc<Config>,
        client: AsyncClient,
        stream_metrics_rx: Receiver<StreamMetrics>,
        serializer_metrics_rx: Receiver<SerializerMetrics>,
        mqtt_metrics_rx: Receiver<MqttMetrics>,
        actions_log: ActionsLog,
    ) -> Monitor {
        Monitor {
            config,
            client,
            stream_metrics_rx,
            serializer_metrics_rx,
            mqtt_metrics_rx,
            actions_log,
        }
    }

    pub async fn start(mut self) -> Result<(), Error> {
        let stream_metrics_config = self.config.stream_metrics.clone();
        let stream_metrics_topic = stream_metrics_config.topic;
        let mut stream_metrics = Vec::with_capacity(10);

        let serializer_metrics_config = self.config.serializer_metrics.clone();
        let serializer_metrics_topic = serializer_metrics_config.topic;
        let mut serializer_metrics = Vec::with_capacity(10);

        let mqtt_metrics_config = self.config.mqtt_metrics.clone();
        let mqtt_metrics_topic = mqtt_metrics_config.topic;
        let mut mqtt_metrics = Vec::with_capacity(10);

        let actions_log_config = self.config.actions_log.clone();
        let actions_log_topic = actions_log_config.topic;
        let mut actions_log_interval =
            IntervalStream::new(interval(Duration::from_secs(actions_log_config.interval)));

        loop {
            select! {
                o = self.stream_metrics_rx.recv_async() => {
                    let o = o?;

                    if stream_metrics_config.blacklist.contains(o.stream()) {
                        continue;
                    }

                    stream_metrics.push(o);
                    let v = serde_json::to_string(&stream_metrics).unwrap();

                    stream_metrics.clear();
                    self.client.publish(&stream_metrics_topic, QoS::AtLeastOnce, false, v).await.unwrap();
                }
                o = self.serializer_metrics_rx.recv_async() => {
                    let o = o?;
                    serializer_metrics.push(o);
                    let v = serde_json::to_string(&serializer_metrics).unwrap();
                    serializer_metrics.clear();
                    self.client.publish(&serializer_metrics_topic, QoS::AtLeastOnce, false, v).await.unwrap();
                }
                o = self.mqtt_metrics_rx.recv_async() => {
                    let o = o?;
                    mqtt_metrics.push(o);
                    let v = serde_json::to_string(&mqtt_metrics).unwrap();
                    mqtt_metrics.clear();
                    self.client.publish(&mqtt_metrics_topic, QoS::AtLeastOnce, false, v).await.unwrap();
                }
                _ = actions_log_interval.next() => {
                    let actions_log = self.actions_log.flush();
                    if actions_log.is_empty() {
                        continue
                    }
                    let v = serde_json::to_string(&actions_log).unwrap();
                    self.client.publish(&actions_log_topic, QoS::AtLeastOnce, false, v).await.unwrap();
                }
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum MqttError {
    #[error("SendError(..)")]
    Send(Request),
    #[error("TrySendError(..)")]
    TrySend(Request),
}

impl From<ClientError> for MqttError {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Request(r) => MqttError::Send(r),
            ClientError::TryRequest(r) => MqttError::TrySend(r),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Collector recv error {0}")]
    Collector(#[from] RecvError),
    #[error("Serde error {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Io error {0}")]
    Io(#[from] io::Error),
    #[error("Mqtt client error {0}")]
    Client(#[from] MqttError),
}
