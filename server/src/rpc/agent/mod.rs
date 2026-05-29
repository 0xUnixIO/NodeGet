use crate::monitoring_push::{DynamicPushRegistry, DynamicSummaryEvent};
use crate::rpc::RpcHelper;
use crate::rpc::{rpc_exec, token_identity};
use crate::token::get::check_token_limit;
use jsonrpsee::PendingSubscriptionSink;
use jsonrpsee::SubscriptionMessage;
use jsonrpsee::core::{JsonRawValue, RpcResult, SubscriptionResult, async_trait};
use jsonrpsee::proc_macros::rpc;
use nodeget_lib::monitoring::data_structure::{
    DynamicMonitoringData, DynamicMonitoringSummaryData, StaticMonitoringData,
};
use nodeget_lib::monitoring::query::{
    DynamicDataAvgQuery, DynamicDataQuery, DynamicDataQueryField, DynamicSummaryAvgQuery,
    DynamicSummaryBucketsMultiQuery, DynamicSummaryBucketsQuery, DynamicSummaryHistoryMultiQuery,
    DynamicSummaryQuery, DynamicSummaryQueryField, QueryCondition, StaticDataAvgQuery,
    StaticDataQuery, StaticDataQueryField,
};
use nodeget_lib::permission::data_structure::{DynamicMonitoringSummary, Permission, Scope};
use nodeget_lib::permission::token_auth::TokenOrAuth;
use nodeget_lib::utils::JsonError;
use serde_json::value::RawValue;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use tracing::Instrument;
use uuid::Uuid;

/// 用于 avg 查询的错误计数器，生成可追踪的错误 ID
static AVG_ERROR_COUNTER: AtomicU64 = AtomicU64::new(0);
pub fn generate_avg_error_id() -> u64 {
    AVG_ERROR_COUNTER.fetch_add(1, Ordering::Relaxed)
}

mod delete_common;
mod delete_dynamic;
mod delete_dynamic_summary;
mod delete_static;
mod query_dynamic;
mod query_dynamic_multi_last;
pub mod query_dynamic_summary;
mod query_dynamic_summary_avg;
mod query_dynamic_summary_buckets;
mod query_dynamic_summary_buckets_multi;
mod query_dynamic_summary_history_multi;
mod query_dynamic_summary_multi_last;
mod query_static;
mod query_static_multi_last;
mod report_dynamic;
mod report_dynamic_summary;
mod report_static;

#[rpc(server, namespace = "agent")]
pub trait Rpc {
    #[method(name = "report_static")]
    async fn report_static(
        &self,
        token: String,
        static_monitoring_data: StaticMonitoringData,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "report_dynamic")]
    async fn report_dynamic(
        &self,
        token: String,
        dynamic_monitoring_data: DynamicMonitoringData,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "query_static")]
    async fn query_static(
        &self,
        token: String,
        static_data_query: StaticDataQuery,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "query_dynamic")]
    async fn query_dynamic(
        &self,
        token: String,
        dynamic_data_query: DynamicDataQuery,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "static_data_multi_last_query")]
    async fn static_data_multi_last_query(
        &self,
        token: String,
        uuids: Vec<Uuid>,
        fields: Vec<StaticDataQueryField>,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "dynamic_data_multi_last_query")]
    async fn dynamic_data_multi_last_query(
        &self,
        token: String,
        uuids: Vec<Uuid>,
        fields: Vec<DynamicDataQueryField>,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "delete_static")]
    async fn delete_static(
        &self,
        token: String,
        conditions: Vec<QueryCondition>,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "delete_dynamic")]
    async fn delete_dynamic(
        &self,
        token: String,
        conditions: Vec<QueryCondition>,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "report_dynamic_summary")]
    async fn report_dynamic_summary(
        &self,
        token: String,
        data: DynamicMonitoringSummaryData,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "query_dynamic_summary")]
    async fn query_dynamic_summary(
        &self,
        token: String,
        query: DynamicSummaryQuery,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "query_dynamic_summary_history_multi")]
    async fn query_dynamic_summary_history_multi(
        &self,
        token: String,
        query: DynamicSummaryHistoryMultiQuery,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "query_dynamic_summary_avg")]
    async fn query_dynamic_summary_avg(
        &self,
        token: String,
        query: DynamicSummaryAvgQuery,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "query_dynamic_summary_buckets")]
    async fn query_dynamic_summary_buckets(
        &self,
        token: String,
        query: DynamicSummaryBucketsQuery,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "query_dynamic_summary_buckets_multi")]
    async fn query_dynamic_summary_buckets_multi(
        &self,
        token: String,
        query: DynamicSummaryBucketsMultiQuery,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "dynamic_summary_multi_last_query")]
    async fn dynamic_summary_multi_last_query(
        &self,
        token: String,
        uuids: Vec<Uuid>,
        fields: Vec<DynamicSummaryQueryField>,
    ) -> RpcResult<Box<RawValue>>;

    #[method(name = "delete_dynamic_summary")]
    async fn delete_dynamic_summary(
        &self,
        token: String,
        conditions: Vec<QueryCondition>,
    ) -> RpcResult<Box<RawValue>>;

    /// 订阅全局动态监控摘要实时推送。
    ///
    /// 浏览器调用方法名：`agent_subscribe_dynamic_summary`
    /// 取消订阅方法名：`agent_unsubscribe_dynamic_summary`
    #[subscription(
        name = "subscribe_dynamic_summary",
        item = DynamicSummaryEvent,
        unsubscribe = "unsubscribe_dynamic_summary"
    )]
    async fn subscribe_dynamic_summary(&self, token: String) -> SubscriptionResult;
}

pub struct AgentRpcImpl;

impl RpcHelper for AgentRpcImpl {}

#[async_trait]
impl RpcServer for AgentRpcImpl {
    async fn report_static(
        &self,
        token: String,
        static_monitoring_data: StaticMonitoringData,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::report_static", token_key = tk, username = un, uuid = %static_monitoring_data.uuid);
        async { rpc_exec!(report_static::report_static(token, static_monitoring_data).await) }
            .instrument(span)
            .await
    }

    async fn report_dynamic(
        &self,
        token: String,
        dynamic_monitoring_data: DynamicMonitoringData,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::report_dynamic", token_key = tk, username = un, uuid = %dynamic_monitoring_data.uuid);
        async { rpc_exec!(report_dynamic::report_dynamic(token, dynamic_monitoring_data).await) }
            .instrument(span)
            .await
    }

    async fn query_static(
        &self,
        token: String,
        static_data_query: StaticDataQuery,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::query_static", token_key = tk, username = un, query = ?static_data_query);
        async { rpc_exec!(query_static::query_static(token, static_data_query).await) }
            .instrument(span)
            .await
    }

    async fn query_dynamic(
        &self,
        token: String,
        dynamic_data_query: DynamicDataQuery,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::query_dynamic", token_key = tk, username = un, query = ?dynamic_data_query);
        async { rpc_exec!(query_dynamic::query_dynamic(token, dynamic_data_query).await) }
            .instrument(span)
            .await
    }

    async fn static_data_multi_last_query(
        &self,
        token: String,
        uuids: Vec<Uuid>,
        fields: Vec<StaticDataQueryField>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::static_data_multi_last_query", token_key = tk, username = un, uuids = ?uuids, fields = ?fields);
        async {
            rpc_exec!(
                query_static_multi_last::static_data_multi_last_query(token, uuids, fields).await
            )
        }
        .instrument(span)
        .await
    }

    async fn dynamic_data_multi_last_query(
        &self,
        token: String,
        uuids: Vec<Uuid>,
        fields: Vec<DynamicDataQueryField>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::dynamic_data_multi_last_query", token_key = tk, username = un, uuids = ?uuids, fields = ?fields);
        async {
            rpc_exec!(
                query_dynamic_multi_last::dynamic_data_multi_last_query(token, uuids, fields).await
            )
        }
        .instrument(span)
        .await
    }

    async fn delete_static(
        &self,
        token: String,
        conditions: Vec<QueryCondition>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::delete_static", token_key = tk, username = un, conditions = ?conditions);
        async { rpc_exec!(delete_static::delete_static(token, conditions).await) }
            .instrument(span)
            .await
    }

    async fn delete_dynamic(
        &self,
        token: String,
        conditions: Vec<QueryCondition>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::delete_dynamic", token_key = tk, username = un, conditions = ?conditions);
        async { rpc_exec!(delete_dynamic::delete_dynamic(token, conditions).await) }
            .instrument(span)
            .await
    }

    async fn report_dynamic_summary(
        &self,
        token: String,
        data: DynamicMonitoringSummaryData,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::report_dynamic_summary", token_key = tk, username = un, uuid = %data.uuid);
        async { rpc_exec!(report_dynamic_summary::report_dynamic_summary(token, data).await) }
            .instrument(span)
            .await
    }

    async fn query_dynamic_summary(
        &self,
        token: String,
        query: DynamicSummaryQuery,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::query_dynamic_summary", token_key = tk, username = un, query = ?query);
        async { rpc_exec!(query_dynamic_summary::query_dynamic_summary(token, query).await) }
            .instrument(span)
            .await
    }

    async fn query_dynamic_summary_history_multi(
        &self,
        token: String,
        query: DynamicSummaryHistoryMultiQuery,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::query_dynamic_summary_history_multi", token_key = tk, username = un, uuids_count = query.uuids.len());
        async {
            rpc_exec!(
                query_dynamic_summary_history_multi::query_dynamic_summary_history_multi(
                    token, query
                )
                .await
            )
        }
        .instrument(span)
        .await
    }

    async fn query_dynamic_summary_avg(
        &self,
        token: String,
        query: DynamicSummaryAvgQuery,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::query_dynamic_summary_avg", token_key = tk, username = un, query = ?query);
        async {
            rpc_exec!(query_dynamic_summary_avg::query_dynamic_summary_avg(token, query).await)
        }
        .instrument(span)
        .await
    }

    async fn query_dynamic_summary_buckets(
        &self,
        token: String,
        query: DynamicSummaryBucketsQuery,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::query_dynamic_summary_buckets", token_key = tk, username = un, uuid = %query.uuid, buckets = query.buckets);
        async {
            rpc_exec!(
                query_dynamic_summary_buckets::query_dynamic_summary_buckets(token, query).await
            )
        }
        .instrument(span)
        .await
    }

    async fn query_dynamic_summary_buckets_multi(
        &self,
        token: String,
        query: DynamicSummaryBucketsMultiQuery,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::query_dynamic_summary_buckets_multi", token_key = tk, username = un, uuids_count = query.uuids.len(), buckets = query.buckets);
        async {
            rpc_exec!(
                query_dynamic_summary_buckets_multi::query_dynamic_summary_buckets_multi(
                    token, query
                )
                .await
            )
        }
        .instrument(span)
        .await
    }

    async fn dynamic_summary_multi_last_query(
        &self,
        token: String,
        uuids: Vec<Uuid>,
        fields: Vec<DynamicSummaryQueryField>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::dynamic_summary_multi_last_query", token_key = tk, username = un, uuids = ?uuids, fields = ?fields);
        async {
            rpc_exec!(
                query_dynamic_summary_multi_last::dynamic_summary_multi_last_query(
                    token, uuids, fields
                )
                .await
            )
        }
        .instrument(span)
        .await
    }

    async fn delete_dynamic_summary(
        &self,
        token: String,
        conditions: Vec<QueryCondition>,
    ) -> RpcResult<Box<RawValue>> {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(target: "monitoring", "agent::delete_dynamic_summary", token_key = tk, username = un, conditions = ?conditions);
        async { rpc_exec!(delete_dynamic_summary::delete_dynamic_summary(token, conditions).await) }
            .instrument(span)
            .await
    }

    async fn subscribe_dynamic_summary(
        &self,
        subscription_sink: PendingSubscriptionSink,
        token: String,
    ) -> SubscriptionResult {
        let (tk, un) = token_identity(&token);
        let span = tracing::info_span!(
            target: "monitoring",
            "agent::subscribe_dynamic_summary",
            token_key = tk,
            username = un,
        );
        let _guard = span.enter();

        tracing::info!(target: "monitoring", "subscribe_dynamic_summary: subscription requested");

        // 解析 token
        let Ok(token_or_auth) = TokenOrAuth::from_full_token(&token) else {
            tracing::error!(target: "monitoring", "subscribe_dynamic_summary: token parse error, rejecting");
            subscription_sink
                .reject(jsonrpsee::types::ErrorObject::borrowed(
                    101,
                    "Token Parse Error",
                    None,
                ))
                .await;
            return Ok(());
        };

        // 权限检查：Global scope + DynamicMonitoringSummary::Read
        let is_allowed_result = check_token_limit(
            &token_or_auth,
            vec![Scope::Global],
            vec![Permission::DynamicMonitoringSummary(
                DynamicMonitoringSummary::Read,
            )],
        )
        .await;

        match is_allowed_result {
            Ok(true) => {
                tracing::debug!(target: "monitoring", "subscribe_dynamic_summary: permission check passed");
            }
            Ok(false) => {
                tracing::error!(target: "monitoring", "subscribe_dynamic_summary: permission denied");
                subscription_sink
                    .reject(jsonrpsee::types::ErrorObject::borrowed(
                        102,
                        "Permission Denied: Missing DynamicMonitoringSummary Read permission",
                        None,
                    ))
                    .await;
                return Ok(());
            }
            Err(e) => {
                let nodeget_err = nodeget_lib::error::anyhow_to_nodeget_error(&e);
                tracing::error!(target: "monitoring", error = %nodeget_err, "subscribe_dynamic_summary: permission check failed");
                subscription_sink
                    .reject(jsonrpsee::types::ErrorObject::owned(
                        nodeget_err.error_code() as i32,
                        nodeget_err.to_string(),
                        None::<JsonError>,
                    ))
                    .await;
                return Ok(());
            }
        }

        // 接受订阅
        let sink = subscription_sink.accept().await?;
        let (tx, mut rx) = mpsc::channel::<DynamicSummaryEvent>(64);
        let reg_id = Uuid::new_v4();

        let registry = DynamicPushRegistry::global();
        registry.subscribe(reg_id, tx).await;
        tracing::info!(target: "monitoring", reg_id = %reg_id, "subscribe_dynamic_summary: subscription accepted");

        // Drop the span guard before spawning
        drop(_guard);
        let forward_span = span.clone();

        tokio::spawn(
            async move {
                while let Some(event) = rx.recv().await {
                    let json_str = match serde_json::to_string(&event) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!(target: "monitoring", error = %e, "subscribe_dynamic_summary: serialize error");
                            break;
                        }
                    };

                    let Ok(raw_value) = JsonRawValue::from_string(json_str) else {
                        tracing::error!(target: "monitoring", "subscribe_dynamic_summary: failed to create JsonRawValue");
                        break;
                    };

                    let sub_msg = SubscriptionMessage::from(raw_value);
                    if sink.send(sub_msg).await.is_err() {
                        break;
                    }
                }

                // WS 断开或序列化失败，清理订阅
                DynamicPushRegistry::global().unsubscribe(&reg_id).await;
                tracing::info!(target: "monitoring", reg_id = %reg_id, "subscribe_dynamic_summary: client disconnected, session removed");
            }
            .instrument(forward_span),
        );

        Ok(())
    }
}
