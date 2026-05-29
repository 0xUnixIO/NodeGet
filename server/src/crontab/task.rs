use crate::entity::{crontab_result, task};
use crate::rpc::RpcHelper;
use crate::rpc::task::{TaskManager, TaskRpcImpl};
use chrono::Utc;
use nodeget_lib::error::NodegetError;
use nodeget_lib::task::{TaskEvent, TaskEventType};
use nodeget_lib::utils::generate_random_string;
use sea_orm::{ActiveValue, EntityTrait, Set};
use tracing::{Instrument, debug, error, info, info_span, warn};
use uuid::Uuid;

pub async fn crontab_task(
    cron_id: i64,
    cron_name: String,
    uuids: Vec<Uuid>,
    task_event_type: TaskEventType,
) {
    let span = info_span!(
        target: "crontab",
        "crontab::dispatch_task",
        cron_id,
        cron_name = %cron_name,
    );

    async {
        let db = match TaskRpcImpl::get_db() {
            Ok(db) => db,
            Err(e) => {
                error!(
                    target: "crontab",
                    error = ?e,
                    "failed to get DB connection for crontab task"
                );
                return;
            }
        };

        // task_event_type 只序列化一次，每次循环直接 clone JSON Value
        let task_type_json = match serde_json::to_value(&task_event_type) {
            Ok(v) => v,
            Err(e) => {
                error!(
                    target: "crontab",
                    error = %e,
                    "failed to serialize task_event_type"
                );
                return;
            }
        };

        let agent_count = uuids.len();
        info!(
            target: "crontab",
            agent_count,
            task_type = ?task_event_type,
            "dispatching task to agents"
        );

        // 收集 crontab_result 日志，最后批量写入（减少 DB 往返：2n 次 → n+1 次）
        let mut pending_logs: Vec<crontab_result::ActiveModel> = Vec::with_capacity(agent_count);
        let run_time = Utc::now().timestamp_millis();

        for uuid in uuids {
            let process_logic = async {
                let token = generate_random_string(10);

                let in_data = task::ActiveModel {
                    id: ActiveValue::default(),
                    uuid: Set(uuid),
                    token: Set(token.clone()),
                    cron_source: Set(Some(cron_name.clone())),
                    timestamp: Set(None),
                    success: Set(None),
                    error_message: Set(None),
                    // 直接 clone 已序列化好的 JSON，避免重复序列化
                    task_event_type: Set(task_type_json.clone()),
                    task_event_result: Set(None),
                };

                let result = task::Entity::insert(in_data).exec(db).await.map_err(|e| {
                    error!(
                        target: "crontab",
                        agent_uuid = %uuid,
                        error = %e,
                        "database insert error"
                    );
                    NodegetError::DatabaseError(format!("Database insert error: {e}"))
                })?;

                let task_id = result.last_insert_id;
                debug!(
                    target: "crontab",
                    agent_uuid = %uuid,
                    task_id,
                    "task record inserted"
                );

                let task = TaskEvent {
                    task_id: task_id.cast_unsigned(),
                    task_token: token,
                    task_event_type: task_event_type.clone(),
                };

                let manager = TaskManager::global();

                match manager.send_event(uuid, task).await {
                    Ok(()) => {
                        info!(
                            target: "crontab",
                            agent_uuid = %uuid,
                            task_id,
                            "task event sent to agent"
                        );
                        Ok(task_id)
                    }
                    Err(e) => {
                        let _ = task::Entity::delete_by_id(task_id).exec(db).await.map_err(
                            |del_err| {
                                error!(
                                    target: "crontab",
                                    agent_uuid = %uuid,
                                    task_id,
                                    error = %del_err,
                                    "database delete error during rollback"
                                );
                                NodegetError::DatabaseError(format!(
                                    "Database delete error: {del_err}"
                                ))
                            },
                        );
                        error!(
                            target: "crontab",
                            agent_uuid = %uuid,
                            task_id,
                            error = %e.1,
                            "failed to send task event to agent"
                        );
                        Err(NodegetError::AgentConnectionError(format!(
                            "Error sending task event: {}",
                            e.1
                        )))
                    }
                }
            };

            let (success, message, task_id) = match process_logic.await {
                Ok(new_id) => (
                    true,
                    format!("任务下发成功，Agent：[{uuid}]，relative_id：{new_id}"),
                    Some(new_id),
                ),
                Err(e) => {
                    warn!(
                        target: "crontab",
                        agent_uuid = %uuid,
                        error = %e,
                        "task dispatch failed"
                    );
                    (
                        false,
                        format!("任务下发失败，Agent：[{uuid}]，错误：{e}"),
                        None,
                    )
                }
            };

            pending_logs.push(crontab_result::ActiveModel {
                id: ActiveValue::NotSet,
                cron_id: Set(cron_id),
                cron_name: Set(cron_name.clone()),
                relative_id: Set(task_id),
                run_time: Set(Some(run_time)),
                success: Set(Some(success)),
                message: Set(Some(message)),
            });
        }

        // 批量写入所有 crontab_result，30 条只需 1 次 DB 往返
        if !pending_logs.is_empty() {
            if let Err(e) = crontab_result::Entity::insert_many(pending_logs)
                .exec(db)
                .await
            {
                error!(
                    target: "crontab",
                    cron_id,
                    error = %e,
                    "failed to batch save crontab_result"
                );
            }
        }
    }
    .instrument(span)
    .await;
}
