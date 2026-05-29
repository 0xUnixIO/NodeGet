use crate::entity::dynamic_monitoring_summary;
use crate::monitoring_uuid_cache::MonitoringUuidCache;
use crate::rpc::RpcHelper;
use crate::rpc::agent::AgentRpcImpl;
use crate::rpc::agent::query_dynamic_summary::{execute_query, field_to_column};
use crate::token::get::check_token_limit;
use jsonrpsee::core::RpcResult;
use nodeget_lib::error::NodegetError;
use nodeget_lib::monitoring::query::{DynamicSummaryHistoryMultiQuery, DynamicSummaryQueryField};
use nodeget_lib::permission::data_structure::{DynamicMonitoringSummary, Permission, Scope};
use nodeget_lib::permission::token_auth::TokenOrAuth;
use nodeget_lib::utils::error_message::anyhow_error_to_raw;
use sea_orm::{ColumnTrait, EntityTrait, ExprTrait, Order, QueryFilter, QueryOrder, QuerySelect};
use serde_json::value::RawValue;
use tracing::debug;

const MAX_UUIDS: usize = 500;
const MAX_LIMIT: u64 = 50_000;

pub async fn query_dynamic_summary_history_multi(
    token: String,
    query: DynamicSummaryHistoryMultiQuery,
) -> RpcResult<Box<RawValue>> {
    let process_logic = async {
        debug!(
            target: "monitoring",
            uuids_count = query.uuids.len(),
            from = query.from,
            to = query.to,
            fields_count = query.fields.len(),
            "Dynamic summary history multi query requested"
        );

        if query.uuids.is_empty() {
            return RawValue::from_string("[]".to_owned())
                .map_err(|e| NodegetError::SerializationError(e.to_string()).into());
        }
        if query.uuids.len() > MAX_UUIDS {
            return Err(NodegetError::InvalidInput(format!(
                "uuids count exceeds maximum ({MAX_UUIDS})"
            ))
            .into());
        }
        if query.from >= query.to {
            return Err(NodegetError::InvalidInput("from must be less than to".to_owned()).into());
        }

        let token_or_auth = TokenOrAuth::from_full_token(&token)
            .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

        let scopes: Vec<Scope> = query.uuids.iter().map(|u| Scope::AgentUuid(*u)).collect();
        let is_allowed = check_token_limit(
            &token_or_auth,
            scopes,
            vec![Permission::DynamicMonitoringSummary(
                DynamicMonitoringSummary::Read,
            )],
        )
        .await?;

        if !is_allowed {
            return Err(NodegetError::PermissionDenied(
                "Permission Denied: Missing DynamicMonitoringSummary Read permission".to_owned(),
            )
            .into());
        }

        let db = AgentRpcImpl::get_db()?;
        let uuid_cache = MonitoringUuidCache::global();

        let mut uuid_ids: Vec<i16> = Vec::with_capacity(query.uuids.len());
        for uuid in &query.uuids {
            let uuid_id = uuid_cache.get_id(uuid).await.ok_or_else(|| {
                NodegetError::NotFound(format!(
                    "Agent UUID {uuid} not found in monitoring registry"
                ))
            })?;
            uuid_ids.push(uuid_id);
        }

        let base = dynamic_monitoring_summary::Entity::find()
            .select_only()
            .column(dynamic_monitoring_summary::Column::UuidId)
            .column(dynamic_monitoring_summary::Column::Timestamp);

        let base = if query.fields.is_empty() {
            add_all_columns(base)
        } else {
            query
                .fields
                .iter()
                .fold(base, |q, f| q.column(field_to_column(f)))
        };

        let row_count = uuid_ids.len() as u64 * 1_000;
        let cap = std::cmp::Ord::min(row_count, MAX_LIMIT);

        let q = base
            .filter(dynamic_monitoring_summary::Column::UuidId.is_in(uuid_ids))
            .filter(
                dynamic_monitoring_summary::Column::Timestamp
                    .gte(query.from)
                    .and(dynamic_monitoring_summary::Column::Timestamp.lte(query.to)),
            )
            .order_by(dynamic_monitoring_summary::Column::Timestamp, Order::Asc)
            .limit(cap);

        execute_query(db, q.into_json(), cap, uuid_cache).await
    };

    match process_logic.await {
        Ok(result) => Ok(result),
        Err(e) => {
            let raw = anyhow_error_to_raw(&e).unwrap_or_else(|_| {
                RawValue::from_string(
                    r#"{"error_id":999,"error_message":"Internal error"}"#.to_owned(),
                )
                .unwrap_or_else(|_| RawValue::from_string("null".to_owned()).unwrap())
            });
            let nodeget_err = nodeget_lib::error::anyhow_to_nodeget_error(&e);
            let json_str = raw.get();
            Err(jsonrpsee::types::ErrorObject::owned(
                nodeget_err.error_code() as i32,
                format!("{nodeget_err}"),
                Some(json_str),
            ))
        }
    }
}

fn add_all_columns(
    q: sea_orm::Select<dynamic_monitoring_summary::Entity>,
) -> sea_orm::Select<dynamic_monitoring_summary::Entity> {
    use DynamicSummaryQueryField::*;
    [
        CpuUsage,
        GpuUsage,
        UsedSwap,
        TotalSwap,
        UsedMemory,
        TotalMemory,
        AvailableMemory,
        LoadOne,
        LoadFive,
        LoadFifteen,
        Uptime,
        BootTime,
        ProcessCount,
        TotalSpace,
        AvailableSpace,
        ReadSpeed,
        WriteSpeed,
        TcpConnections,
        UdpConnections,
        TotalReceived,
        TotalTransmitted,
        TransmitSpeed,
        ReceiveSpeed,
    ]
    .iter()
    .fold(q, |acc, f| acc.column(field_to_column(f)))
}
