//! 全局动态监控数据推送注册表。
//!
//! 当 Agent 上报 dynamic summary 数据后，通过 [`DynamicPushRegistry::broadcast`]
//! 向所有已订阅的 WebSocket 客户端推送 [`DynamicSummaryEvent`]。
//!
//! channel 满时用 `try_send` 丢弃（监控数据高频，下一帧会补上）。

use nodeget_lib::monitoring::data_structure::DynamicMonitoringSummaryData;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

/// 推送给浏览器的动态监控摘要事件。
///
/// 字段与 [`DynamicMonitoringSummaryData`] 一一对应，但：
/// - 增加了 `uuid: String`（全局订阅，需区分 agent）
/// - `cpu_usage` / `gpu_usage` / `load_one` / `load_five` / `load_fifteen`
///   已做 descale（÷10），使用 `Option<f64>`
/// - `time` 是有符号 i64（milliseconds，与 DB 返回一致）
#[derive(Clone, Debug, Serialize)]
pub struct DynamicSummaryEvent {
    pub uuid: String,
    pub time: i64,
    pub cpu_usage: Option<f64>,
    pub gpu_usage: Option<f64>,
    pub used_swap: Option<i64>,
    pub total_swap: Option<i64>,
    pub used_memory: Option<i64>,
    pub total_memory: Option<i64>,
    pub available_memory: Option<i64>,
    pub load_one: Option<f64>,
    pub load_five: Option<f64>,
    pub load_fifteen: Option<f64>,
    pub uptime: Option<i32>,
    pub boot_time: Option<i64>,
    pub process_count: Option<i32>,
    pub total_space: Option<i64>,
    pub available_space: Option<i64>,
    pub read_speed: Option<i64>,
    pub write_speed: Option<i64>,
    pub tcp_connections: Option<i32>,
    pub udp_connections: Option<i32>,
    pub total_received: Option<i64>,
    pub total_transmitted: Option<i64>,
    pub transmit_speed: Option<i64>,
    pub receive_speed: Option<i64>,
}

impl DynamicSummaryEvent {
    /// 从 `DynamicMonitoringSummaryData` 构造事件，并对 *10 缩放字段进行 descale。
    ///
    /// `cpu_usage`、`gpu_usage`、`load_one`、`load_five`、`load_fifteen`
    /// 在数据库中均以 `i16 * 10` 存储，读取时需要除以 10 还原为真实浮点数。
    pub fn from_data(uuid: Uuid, time: i64, data: &DynamicMonitoringSummaryData) -> Self {
        Self {
            uuid: uuid.to_string(),
            time,
            // *10 缩放字段：转换为 f64 并 ÷10
            cpu_usage: data.cpu_usage.map(|v| f64::from(v) / 10.0),
            gpu_usage: data.gpu_usage.map(|v| f64::from(v) / 10.0),
            load_one: data.load_one.map(|v| f64::from(v) / 10.0),
            load_five: data.load_five.map(|v| f64::from(v) / 10.0),
            load_fifteen: data.load_fifteen.map(|v| f64::from(v) / 10.0),
            // 原始 i64 字段，直接传递
            used_swap: data.used_swap,
            total_swap: data.total_swap,
            used_memory: data.used_memory,
            total_memory: data.total_memory,
            available_memory: data.available_memory,
            uptime: data.uptime,
            boot_time: data.boot_time,
            process_count: data.process_count,
            total_space: data.total_space,
            available_space: data.available_space,
            read_speed: data.read_speed,
            write_speed: data.write_speed,
            tcp_connections: data.tcp_connections,
            udp_connections: data.udp_connections,
            total_received: data.total_received,
            total_transmitted: data.total_transmitted,
            transmit_speed: data.transmit_speed,
            receive_speed: data.receive_speed,
        }
    }
}

// ── 全局单例 ──────────────────────────────────────────────────────────────

static GLOBAL_REGISTRY: OnceLock<Arc<DynamicPushRegistry>> = OnceLock::new();

/// 全局动态监控推送注册表，维护所有活跃 WebSocket 订阅者的 sender。
pub struct DynamicPushRegistry {
    subscribers: Arc<RwLock<HashMap<Uuid, mpsc::Sender<DynamicSummaryEvent>>>>,
}

impl DynamicPushRegistry {
    /// 获取全局单例，不存在时自动初始化。
    pub fn global() -> Arc<Self> {
        GLOBAL_REGISTRY
            .get_or_init(|| {
                Arc::new(Self {
                    subscribers: Arc::new(RwLock::new(HashMap::new())),
                })
            })
            .clone()
    }

    /// 注册一个新订阅者。
    pub async fn subscribe(&self, reg_id: Uuid, tx: mpsc::Sender<DynamicSummaryEvent>) {
        let mut subs = self.subscribers.write().await;
        subs.insert(reg_id, tx);
    }

    /// 移除一个订阅者（WS 断开时调用）。
    pub async fn unsubscribe(&self, reg_id: &Uuid) {
        let mut subs = self.subscribers.write().await;
        subs.remove(reg_id);
    }

    /// 向所有订阅者广播事件，channel 满时直接丢弃（不阻塞上报路径）。
    pub async fn broadcast(&self, event: DynamicSummaryEvent) {
        let subs = self.subscribers.read().await;
        for tx in subs.values() {
            let _ = tx.try_send(event.clone());
        }
    }
}
