use std::io;
use std::sync::Arc;

use flume::{Receiver, RecvError};
use rumqttc::{AsyncClient, ClientError, QoS, Request};
use tokio::select;

use crate::base::bridge::StreamMetrics;
use crate::Config;

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
}

impl Monitor {
    pub fn new(
        config: Arc<Config>,
        client: AsyncClient,
        stream_metrics_rx: Receiver<StreamMetrics>,
        serializer_metrics_rx: Receiver<SerializerMetrics>,
    ) -> Monitor {
        Monitor { config, client, stream_metrics_rx, serializer_metrics_rx }
    }

    pub async fn start(&self) -> Result<(), Error> {
        let stream_metrics_config = self.config.stream_metrics.clone();
        let stream_metrics_topic = stream_metrics_config.topic;
        let mut stream_metrics = Vec::with_capacity(10);

        let serializer_metrics_config = self.config.serializer_metrics.clone();
        let serializer_metrics_topic = serializer_metrics_config.topic;
        let mut serializer_metrics = Vec::with_capacity(10);

        loop {
            select! {
                o = self.stream_metrics_rx.recv_async() => {
                    let o = o?;
                    stream_metrics.push(o);
                    let v = serde_json::to_string(&stream_metrics).unwrap();
                    println!("Received {:?}", v);

                    stream_metrics.clear();
                    self.client.publish(&stream_metrics_topic, QoS::AtLeastOnce, false, v).await.unwrap();
                }
                o = self.serializer_metrics_rx.recv_async() => {
                    let o = o?;
                    serializer_metrics.push(o);
                    let v = serde_json::to_string(&serializer_metrics).unwrap();
                    println!("Received {:?}", v);
                    serializer_metrics.clear();
                    self.client.publish(&serializer_metrics_topic, QoS::AtLeastOnce, false, v).await.unwrap();
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