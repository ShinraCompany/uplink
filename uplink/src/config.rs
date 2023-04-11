use std::{collections::HashMap, fmt::Debug};

use crate::bridge::ActionRoute;
use serde::{Deserialize, Serialize};

#[cfg(any(target_os = "linux", target_os = "android"))]
#[derive(Debug, Clone, Deserialize)]
pub struct LoggerConfig {
    pub tags: Vec<String>,
    pub min_level: u8,
    pub stream_size: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Authentication {
    pub ca_certificate: String,
    pub device_certificate: String,
    pub device_private_key: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Stats {
    pub enabled: bool,
    pub process_names: Vec<String>,
    pub update_period: u64,
    pub stream_size: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SimulatorConfig {
    /// path to directory containing files with gps paths to be used in simulation
    pub gps_paths: String,
    /// actions that are to be routed to simulator
    pub actions: Vec<ActionRoute>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DownloaderConfig {
    pub path: String,
    #[serde(default)]
    pub actions: Vec<ActionRoute>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct InstallerConfig {
    pub path: String,
    #[serde(default)]
    pub actions: Vec<ActionRoute>,
    pub uplink_port: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SerializerMetricsConfig {
    pub enabled: bool,
    pub topic: String,
    pub timeout: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct MqttMetricsConfig {
    pub enabled: bool,
    pub topic: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AppConfig {
    pub port: u16,
    #[serde(default)]
    pub actions: Vec<ActionRoute>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TracingConfig {
    pub enabled: bool,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MqttConfig {
    pub max_packet_size: usize,
    pub max_inflight: u16,
    pub keep_alive: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(flatten)]
    pub bridge: crate::bridge::Config,
    #[serde(flatten)]
    pub serializer: crate::serializer::Config,
    pub broker: String,
    pub port: u16,
    #[serde(default)]
    pub apis: TracingConfig,
    pub authentication: Option<Authentication>,
    pub tcpapps: HashMap<String, AppConfig>,
    pub mqtt: MqttConfig,
    #[serde(default)]
    pub processes: Vec<ActionRoute>,
    #[serde(skip)]
    pub actions_subscription: String,
    pub serializer_metrics: SerializerMetricsConfig,
    pub mqtt_metrics: MqttMetricsConfig,
    pub downloader: DownloaderConfig,
    pub system_stats: Stats,
    pub simulator: Option<SimulatorConfig>,
    pub ota_installer: InstallerConfig,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub logging: Option<LoggerConfig>,
}
