mod bridge;
mod metrics;
mod stream;

use flume::Sender;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

pub use metrics::StreamMetrics;
use std::fmt::Debug;
pub use stream::{Error, Stream, StreamStatus};

use crate::Action;

pub trait Point: Send + Debug {
    fn sequence(&self) -> u32;
    fn timestamp(&self) -> u64;
}

pub trait Package: Send + Debug {
    fn topic(&self) -> Arc<String>;
    fn stream(&self) -> Arc<String>;
    // TODO: Implement a generic Return type that can wrap
    // around custom serialization error types.
    fn serialize(&self) -> serde_json::Result<Vec<u8>>;
    fn anomalies(&self) -> Option<(String, usize)>;
    fn len(&self) -> usize;
    fn latency(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// TODO Don't do any deserialization on payload. Read it a Vec<u8> which is in turn a json
// TODO which cloud will double deserialize (Batch 1st and messages next)
#[derive(Debug, Serialize, Deserialize)]
pub struct Payload {
    #[serde(skip_serializing)]
    pub stream: String,
    pub sequence: u32,
    pub timestamp: u64,
    #[serde(flatten)]
    pub payload: Value,
}

impl Point for Payload {
    fn sequence(&self) -> u32 {
        self.sequence
    }

    fn timestamp(&self) -> u64 {
        self.timestamp
    }
}
