use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use flume::Sender;
use log::{error, info};
use tokio::time::{interval, Interval};

use crate::base::bridge::{self, StreamMetrics, StreamStatus};
use crate::{Config, Package, Payload, Stream};

use super::delaymap::DelayMap;

pub struct Streams {
    config: Arc<Config>,
    data_tx: Sender<Box<dyn Package>>,
    metrics_tx: Sender<StreamMetrics>,
    map: HashMap<String, Stream<Payload>>,
    pub stream_timeouts: DelayMap<String>,
    pub metrics_timeouts: DelayMap<String>,
    pub metrics_timeout: Interval,
}

impl Streams {
    pub async fn new(
        config: Arc<Config>,
        data_tx: Sender<Box<dyn Package>>,
        metrics_tx: Sender<StreamMetrics>,
    ) -> Self {
        let mut map = HashMap::new();
        for (name, stream) in &config.streams {
            let stream = Stream::with_config(name, stream, data_tx.clone());
            map.insert(name.to_owned(), stream);
        }

        let metrics_timeout = interval(Duration::from_secs(config.stream_metrics.timeout));
        Self {
            config,
            data_tx,
            metrics_tx,
            map,
            stream_timeouts: DelayMap::new(),
            metrics_timeouts: DelayMap::new(),
            metrics_timeout,
        }
    }

    pub async fn forward(&mut self, data: Payload) {
        let stream = match self.map.get_mut(&data.stream) {
            Some(partition) => partition,
            None => {
                if self.map.keys().len() > 20 {
                    error!("Failed to create {:?} stream. More than max 20 streams", data.stream);
                    return;
                }

                let stream = Stream::dynamic(
                    &data.stream,
                    &self.config.project_id,
                    &self.config.device_id,
                    self.data_tx.clone(),
                );

                self.map.entry(data.stream.to_owned()).or_insert(stream)
            }
        };

        let max_stream_size = stream.max_buffer_size;
        let state = match stream.fill(data).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to send data. Error = {:?}", e.to_string());
                return;
            }
        };

        // Remove timeout from flush_handler for selected stream if stream state is flushed,
        // do nothing if stream state is partial. Insert a new timeout if initial fill.
        // Warn in case stream flushed stream was not in the queue.
        if max_stream_size > 1 {
            match state {
                StreamStatus::Flushed(name) => self.stream_timeouts.remove(name),
                StreamStatus::Init(name, flush_period) => {
                    info!("Initialized stream buffer for {}", name);
                    self.stream_timeouts.insert(name, flush_period);
                }
                StreamStatus::Partial(_l) => {}
            }
        }
    }

    // Flush stream/partitions that timeout
    pub async fn flush_stream(&mut self, stream: &str) -> Result<(), bridge::Error> {
        let stream = self.map.get_mut(stream).unwrap();
        stream.flush().await?;
        Ok(())
    }

    // Enable actual metrics timers when there is data. This method is called every minute by the bridge
    pub fn check_and_flush_metrics(&mut self) -> Result<(), flume::TrySendError<StreamMetrics>> {
        for (_name, data) in self.map.iter_mut() {
            let metrics = data.metrics.clone();

            // Initialize metrics timeouts when force flush sees data counts
            if metrics.point_count() > 0 {
                self.metrics_tx.try_send(metrics)?;
                data.metrics.prepare_next();
            }
        }

        Ok(())
    }
}
