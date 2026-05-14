//! 全局动态监控数据推送注册表。
//!
//! 当 Agent 上报 dynamic summary 数据后，通过 [`DynamicPushRegistry::broadcast`]
//! 向所有已订阅的 WebSocket 客户端推送 [`DynamicSummaryEvent`]。
//!
//! channel 满时用 `try_send` 丢弃（监控数据高频，下一帧会补上）。

use nodeget_lib::monitoring::data_structure::DynamicMonitoringSummaryData;
use serde::Serialize;
use serde_json::Value;
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
/// - `timestamp` 是有符号 i64（milliseconds，与 DB 返回一致）
#[derive(Clone, Debug, Serialize)]
pub struct DynamicSummaryEvent {
    pub uuid: String,
    pub timestamp: i64,
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
    pub fn from_data(uuid: Uuid, timestamp: i64, data: &DynamicMonitoringSummaryData) -> Self {
        Self {
            uuid: uuid.to_string(),
            timestamp,
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
    /// 在线人数订阅者（保留向后兼容）
    viewer_count_subs: Arc<RwLock<HashMap<Uuid, mpsc::Sender<usize>>>>,
    /// 访客统计订阅者：记录新访问或在线人数变化时广播
    visitor_stats_subs: Arc<RwLock<HashMap<Uuid, mpsc::Sender<Value>>>>,
    /// 最新访客统计 JSON 缓存，用于在线人数变化时直接更新 online_viewers 再广播
    latest_visitor_stats: Arc<RwLock<Option<Value>>>,
}

impl DynamicPushRegistry {
    pub fn global() -> Arc<Self> {
        GLOBAL_REGISTRY
            .get_or_init(|| {
                Arc::new(Self {
                    subscribers: Arc::new(RwLock::new(HashMap::new())),
                    viewer_count_subs: Arc::new(RwLock::new(HashMap::new())),
                    visitor_stats_subs: Arc::new(RwLock::new(HashMap::new())),
                    latest_visitor_stats: Arc::new(RwLock::new(None)),
                })
            })
            .clone()
    }

    /// 返回当前动态摘要订阅者数（即"在线人数"）
    pub async fn online_viewers(&self) -> usize {
        self.subscribers.read().await.len()
    }

    /// 注册动态摘要订阅者，并广播最新在线人数（含访客统计更新）。
    pub async fn subscribe(&self, reg_id: Uuid, tx: mpsc::Sender<DynamicSummaryEvent>) {
        let count = {
            let mut subs = self.subscribers.write().await;
            subs.insert(reg_id, tx);
            subs.len()
        };
        self.broadcast_viewer_count(count).await;
    }

    /// 移除动态摘要订阅者，并广播最新在线人数（含访客统计更新）。
    pub async fn unsubscribe(&self, reg_id: &Uuid) {
        let count = {
            let mut subs = self.subscribers.write().await;
            subs.remove(reg_id);
            subs.len()
        };
        self.broadcast_viewer_count(count).await;
    }

    /// 向所有动态摘要订阅者广播事件。
    pub async fn broadcast(&self, event: DynamicSummaryEvent) {
        let subs = self.subscribers.read().await;
        for tx in subs.values() {
            let _ = tx.try_send(event.clone());
        }
    }

    /// 注册在线人数订阅者（向后兼容）。
    pub async fn subscribe_viewer_count(&self, reg_id: Uuid, tx: mpsc::Sender<usize>) {
        let count = self.subscribers.read().await.len();
        let _ = tx.try_send(count);
        self.viewer_count_subs.write().await.insert(reg_id, tx);
    }

    pub async fn unsubscribe_viewer_count(&self, reg_id: &Uuid) {
        self.viewer_count_subs.write().await.remove(reg_id);
    }

    /// 在线人数变化时：推送给 viewer_count_subs，并将 online_viewers 字段注入
    /// 缓存的访客统计后广播给 visitor_stats_subs，无需重查 DB。
    async fn broadcast_viewer_count(&self, count: usize) {
        // 旧订阅者
        {
            let subs = self.viewer_count_subs.read().await;
            for tx in subs.values() {
                let _ = tx.try_send(count);
            }
        }
        // 更新缓存并广播给访客统计订阅者
        let updated = {
            let guard = self.latest_visitor_stats.read().await;
            guard.as_ref().map(|v| {
                let mut updated = v.clone();
                if let Some(obj) = updated.as_object_mut() {
                    obj.insert("online_viewers".to_string(), serde_json::json!(count));
                }
                updated
            })
        };
        if let Some(stats) = updated {
            *self.latest_visitor_stats.write().await = Some(stats.clone());
            let subs = self.visitor_stats_subs.read().await;
            for tx in subs.values() {
                let _ = tx.try_send(stats.clone());
            }
        }
    }

    /// 注册访客统计订阅者。
    pub async fn subscribe_visitor_stats(&self, reg_id: Uuid, tx: mpsc::Sender<Value>) {
        self.visitor_stats_subs.write().await.insert(reg_id, tx);
    }

    pub async fn unsubscribe_visitor_stats(&self, reg_id: &Uuid) {
        self.visitor_stats_subs.write().await.remove(reg_id);
    }

    /// 广播访客统计，同时更新缓存（用于后续在线人数变化时的增量广播）。
    pub async fn broadcast_visitor_stats(&self, stats: &Value) {
        *self.latest_visitor_stats.write().await = Some(stats.clone());
        let subs = self.visitor_stats_subs.read().await;
        for tx in subs.values() {
            let _ = tx.try_send(stats.clone());
        }
    }
}
