use crate::monitoring_uuid_cache::MonitoringUuidCache;
use crate::rpc::RpcHelper;
use crate::rpc::agent::AgentRpcImpl;
use crate::token::get::check_token_limit;
use jsonrpsee::core::RpcResult;
use nodeget_lib::error::NodegetError;
use nodeget_lib::monitoring::query::{DynamicSummaryBucketsMultiQuery, DynamicSummaryQueryField};
use nodeget_lib::permission::data_structure::{DynamicMonitoringSummary, Permission, Scope};
use nodeget_lib::permission::token_auth::TokenOrAuth;
use nodeget_lib::utils::error_message::anyhow_error_to_raw;
use sea_orm::{DatabaseBackend, DatabaseConnection, FromQueryResult, Statement};
use serde_json::Value;
use serde_json::value::RawValue;
use std::fmt::Write;
use tracing::{debug, error};

const MAX_UUIDS: usize = 500;

#[derive(Debug, FromQueryResult)]
struct JsonAggRow {
    data: Value,
}

pub async fn query_dynamic_summary_buckets_multi(
    token: String,
    query: DynamicSummaryBucketsMultiQuery,
) -> RpcResult<Box<RawValue>> {
    let process_logic = async {
        debug!(
            target: "monitoring",
            uuids_count = query.uuids.len(),
            from = query.from,
            to = query.to,
            buckets = query.buckets,
            fields_count = query.fields.len(),
            "Dynamic summary buckets multi query requested"
        );

        if query.uuids.is_empty() {
            let json_str = serde_json::to_string(&Value::Array(Vec::new())).map_err(|e| {
                NodegetError::SerializationError(format!("Serialization failed: {e}"))
            })?;
            return RawValue::from_string(json_str).map_err(|e| {
                NodegetError::SerializationError(format!("RawValue error: {e}")).into()
            });
        }
        if query.uuids.len() > MAX_UUIDS {
            return Err(NodegetError::InvalidInput(format!(
                "uuids count exceeds maximum ({MAX_UUIDS})"
            ))
            .into());
        }
        if query.buckets == 0 {
            return Err(NodegetError::InvalidInput("buckets must be >= 1".to_owned()).into());
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
        ensure_postgres(db)?;

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

        let buckets_i64 = i64::try_from(query.buckets)
            .map_err(|_| NodegetError::InvalidInput("buckets value too large".to_owned()))?;

        // $1=from, $2=to, $3=buckets, $4..$N=uuid_ids（动态 IN 占位符）
        let placeholders: String = (0..uuid_ids.len())
            .map(|i| format!("${}", i + 4))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = build_sql(&query.fields, &placeholders);

        let mut values: Vec<sea_orm::Value> =
            vec![query.from.into(), query.to.into(), buckets_i64.into()];
        for &id in &uuid_ids {
            values.push(id.into());
        }

        let statement = Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values);

        let row = JsonAggRow::find_by_statement(statement)
            .one(db)
            .await
            .map_err(|e| {
                error!(target: "monitoring", error = %e, "Dynamic summary buckets multi DB error");
                NodegetError::DatabaseError(format!("Database error: {e}"))
            })?;

        let json = row.map_or(Value::Array(Vec::new()), |r| r.data);
        let json_str = serde_json::to_string(&json)
            .map_err(|e| NodegetError::SerializationError(format!("Serialization failed: {e}")))?;

        RawValue::from_string(json_str)
            .map_err(|e| NodegetError::SerializationError(format!("RawValue error: {e}")).into())
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

fn ensure_postgres(db: &DatabaseConnection) -> anyhow::Result<()> {
    if db.get_database_backend() == DatabaseBackend::Postgres {
        return Ok(());
    }
    Err(NodegetError::InvalidInput(
        "agent_query_dynamic_summary_buckets_multi only supports PostgreSQL".to_owned(),
    )
    .into())
}

fn build_sql(fields: &[DynamicSummaryQueryField], uuid_placeholders: &str) -> String {
    // raw CTE：每桶的总记录数 + 各字段跨节点 SUM（scaled 字段除以 10 还原）
    let field_aggregates = fields.iter().fold(String::new(), |mut s, f| {
        if f.is_scaled() {
            write!(
                s,
                ",\n        SUM({col})::double precision / 10.0 AS {col}",
                col = f.column_name()
            )
            .unwrap();
        } else {
            write!(
                s,
                ",\n        SUM({col})::double precision AS {col}",
                col = f.column_name()
            )
            .unwrap();
        }
        s
    });

    let field_selects = fields.iter().fold(String::new(), |mut s, f| {
        write!(s, ",\n        raw.{col}", col = f.column_name()).unwrap();
        s
    });

    let json_fields = fields.iter().fold(String::new(), |mut s, f| {
        write!(
            s,
            ", '{key}', {col}",
            key = f.json_key(),
            col = f.column_name()
        )
        .unwrap();
        s
    });

    format!(
        r"
WITH buckets AS (
    SELECT generate_series(0, $3::bigint - 1) AS b
),
raw AS (
    SELECT
        GREATEST(0, LEAST($3::bigint - 1,
            FLOOR(
                (timestamp::float8 - $1::float8) * $3::float8
                / ($2::float8 - $1::float8)
            )::bigint
        )) AS b,
        COUNT(*) AS record_count{field_aggregates}
    FROM dynamic_monitoring_summary
    WHERE uuid_id IN ({uuid_placeholders})
      AND timestamp >= $1
      AND timestamp <= $2
    GROUP BY 1
),
merged AS (
    SELECT
        bkt.b,
        ($1::float8 + (bkt.b::float8 + 0.5) * ($2::float8 - $1::float8) / $3::float8)::bigint AS t,
        COALESCE(raw.record_count, 0) AS count{field_selects}
    FROM buckets bkt
    LEFT JOIN raw ON bkt.b = raw.b
)
SELECT COALESCE(
    jsonb_agg(
        jsonb_build_object('t', t, 'count', count{json_fields})
        ORDER BY b
    ),
    '[]'::jsonb
) AS data
FROM merged
"
    )
}
