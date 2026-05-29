use crate::entity::task;
use crate::rpc::RpcHelper;
use crate::rpc::task::TaskRpcImpl;
use crate::token::get::check_token_limit;
use futures_util::StreamExt;
use jsonrpsee::core::RpcResult;
use nodeget_lib::error::NodegetError;
use nodeget_lib::permission::data_structure::{Permission, Scope, Task};
use nodeget_lib::permission::token_auth::TokenOrAuth;
use nodeget_lib::utils::server_json::{rename_key, try_parse_json_field};
use sea_orm::sea_query::{Alias, BinOper, Expr, Func, LikeExpr, Query};
use sea_orm::{ColumnTrait, DbBackend, EntityTrait, ExprTrait, QueryFilter, QuerySelect};
use serde_json::value::RawValue;
use tracing::{debug, error};

fn escape_like_pattern(pattern: &str) -> String {
    pattern.replace('%', r"\%").replace('_', r"\_")
}

pub async fn query_latest_per_node(token: String, task_type: String) -> RpcResult<Box<RawValue>> {
    let process_logic = async {
        debug!(target: "task", task_type = %task_type, "processing query_latest_per_node request");

        let token_or_auth = TokenOrAuth::from_full_token(&token)
            .map_err(|e| NodegetError::ParseError(format!("Failed to parse token: {e}")))?;

        let is_allowed = check_token_limit(
            &token_or_auth,
            vec![Scope::Global],
            vec![Permission::Task(Task::Read(task_type.clone()))],
        )
        .await?;

        if !is_allowed {
            return Err(NodegetError::PermissionDenied(
                "Permission Denied: Insufficient permissions to read task data".to_owned(),
            )
            .into());
        }

        let db = TaskRpcImpl::get_db()?;

        // 构建类型过滤条件，与 query.rs 的处理方式保持一致
        let type_condition = if db.get_database_backend() == DbBackend::Postgres {
            Expr::col(task::Column::TaskEventType).binary(BinOper::Custom("?"), task_type.clone())
        } else {
            let escaped = escape_like_pattern(&task_type);
            let pattern = format!("%\"{escaped}\":%");
            Expr::col(task::Column::TaskEventType)
                .cast_as(Alias::new("text"))
                .like(LikeExpr::new(pattern).escape('\\'))
        };

        // 子查询：每个 (uuid, cron_source) 组合取 MAX(id)
        // PostgreSQL 和 SQLite 都适用，语义等价于 DISTINCT ON (uuid, cron_source) ORDER BY id DESC
        let subquery = Query::select()
            .expr(Func::max(Expr::col(task::Column::Id)))
            .from(task::Entity)
            .cond_where(type_condition)
            .group_by_columns([task::Column::Uuid, task::Column::CronSource])
            .to_owned();

        // 主查询：WHERE id IN (subquery)，字段列表与 query.rs 保持一致
        let query = task::Entity::find()
            .select_only()
            .column(task::Column::Id)
            .column(task::Column::Uuid)
            .column(task::Column::CronSource)
            .column(task::Column::Timestamp)
            .column(task::Column::Success)
            .column(task::Column::ErrorMessage)
            .column(task::Column::TaskEventType)
            .column(task::Column::TaskEventResult)
            .filter(task::Column::Id.in_subquery(subquery));

        // 流式序列化，与 query.rs 完全一致
        let mut stream = query.into_json().stream(db).await.map_err(|e| {
            error!(target: "task", error = %e, "Database query error");
            NodegetError::DatabaseError(format!("Database query error: {e}"))
        })?;

        let mut output_buffer: Vec<u8> = Vec::with_capacity(4096);
        output_buffer.push(b'[');
        let mut first = true;
        let mut result_count: usize = 0;

        while let Some(item_res) = stream.next().await {
            match item_res {
                Ok(mut v) => {
                    result_count += 1;
                    if let Some(obj) = v.as_object_mut() {
                        rename_key(obj, "id", "task_id");
                        try_parse_json_field(obj, "task_event_type");
                        try_parse_json_field(obj, "task_event_result");
                    }
                    if first {
                        first = false;
                    } else {
                        output_buffer.push(b',');
                    }
                    if let Err(e) = serde_json::to_writer(&mut output_buffer, &v) {
                        error!(target: "task", error = %e, "Serialization failed");
                        return Err(NodegetError::SerializationError(format!(
                            "Serialization failed: {e}"
                        ))
                        .into());
                    }
                }
                Err(e) => {
                    error!(target: "task", error = %e, "Stream read error");
                    return Err(
                        NodegetError::DatabaseError(format!("Stream read error: {e}")).into(),
                    );
                }
            }
        }

        output_buffer.push(b']');
        debug!(target: "task", result_count, "query_latest_per_node completed");

        let json_string = String::from_utf8(output_buffer)
            .map_err(|e| NodegetError::SerializationError(format!("UTF8 conversion error: {e}")))?;

        RawValue::from_string(json_string)
            .map_err(|e| NodegetError::SerializationError(format!("RawValue error: {e}")).into())
    };

    match process_logic.await {
        Ok(result) => Ok(result),
        Err(e) => {
            let raw =
                nodeget_lib::utils::error_message::anyhow_error_to_raw(&e).unwrap_or_else(|_| {
                    RawValue::from_string(
                        r#"{"error_id":999,"error_message":"Internal error"}"#.to_owned(),
                    )
                    .unwrap()
                });
            let nodeget_err = nodeget_lib::error::anyhow_to_nodeget_error(&e);
            Err(jsonrpsee::types::ErrorObject::owned(
                nodeget_err.error_code() as i32,
                format!("{nodeget_err}"),
                Some(raw.get()),
            ))
        }
    }
}
