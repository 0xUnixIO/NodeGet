use axum::http::StatusCode;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    Set,
};
use serde::Serialize;
use tracing::error;

use crate::entity::{visit_daily_stats, visit_log};

#[derive(Serialize)]
struct VisitorStatsResponse {
    today_rank: u64,
    today_total: u64,
    all_time_total: u64,
    yesterday_total: u64,
}

/// X-Real-IP → X-Forwarded-For → ConnectInfo 三级降级
fn extract_ip(req: &axum::extract::Request) -> String {
    if let Some(ip) = req
        .headers()
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
    {
        return ip;
    }
    if let Some(forwarded) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        let first = forwarded.split(',').next().unwrap_or("").trim();
        if !first.is_empty() {
            return first.to_owned();
        }
    }
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map_or_else(|| "127.0.0.1".to_owned(), |info| info.0.ip().to_string())
}

fn today_utc_start_ts() -> i64 {
    chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp()
}

fn ts_to_date_str(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_owned())
}

pub async fn handler(
    req: axum::extract::Request,
) -> axum::http::Response<jsonrpsee::server::HttpBody> {
    if req.method() == axum::http::Method::OPTIONS {
        return axum::http::Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(axum::http::header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS")
            .header(axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS, "*")
            .body(jsonrpsee::server::HttpBody::default())
            .expect("构建 CORS 响应失败");
    }

    let Some(db) = crate::DB.get() else {
        error!(target: "visitor_stats", "数据库未初始化");
        return build_error(StatusCode::INTERNAL_SERVER_ERROR, "数据库未初始化");
    };

    let ip = extract_ip(&req);
    let now_ts = chrono::Utc::now().timestamp();

    // 用 ActiveModel 插入，避免 SQL 注入
    let new_visit = visit_log::ActiveModel {
        ip: Set(ip),
        visited_at: Set(now_ts),
        ..Default::default()
    };
    if let Err(e) = new_visit.insert(db).await {
        error!(target: "visitor_stats", error = %e, "插入 visit_log 失败");
        return build_error(StatusCode::INTERNAL_SERVER_ERROR, "记录访问失败");
    }

    let today_start = today_utc_start_ts();
    let yesterday_start = today_start - 86400;

    // 今日排名 = 今日总访问量（插入后的计数）
    let today_total = visit_log::Entity::find()
        .filter(visit_log::Column::VisitedAt.gte(today_start))
        .count(db)
        .await
        .unwrap_or(0);

    // 全部历史：visit_daily_stats 总和 + visit_log 全部记录（两者不重叠）
    let all_daily = visit_daily_stats::Entity::find()
        .all(db)
        .await
        .unwrap_or_default();
    let daily_sum: i64 = all_daily.iter().map(|m| m.total_count).sum();
    let log_total = visit_log::Entity::find().count(db).await.unwrap_or(0) as i64;
    let all_time_total = (daily_sum + log_total).max(0) as u64;

    // 昨日：优先查 daily_stats，不存在则查 visit_log（尚未聚合时）
    let yesterday_date = ts_to_date_str(yesterday_start);
    let yesterday_total = match visit_daily_stats::Entity::find_by_id(&yesterday_date)
        .one(db)
        .await
        .unwrap_or(None)
    {
        Some(m) => m.total_count.max(0) as u64,
        None => visit_log::Entity::find()
            .filter(visit_log::Column::VisitedAt.gte(yesterday_start))
            .filter(visit_log::Column::VisitedAt.lt(today_start))
            .count(db)
            .await
            .unwrap_or(0),
    };

    let resp = VisitorStatsResponse {
        today_rank: today_total,
        today_total,
        all_time_total,
        yesterday_total,
    };

    match serde_json::to_string(&resp) {
        Ok(body) => axum::http::Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")
            .header(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(jsonrpsee::server::HttpBody::from(body))
            .expect("构建响应失败"),
        Err(e) => {
            error!(target: "visitor_stats", error = %e, "序列化响应失败");
            build_error(StatusCode::INTERNAL_SERVER_ERROR, "序列化失败")
        }
    }
}

/// 聚合 visit_log 中今天之前的数据到 visit_daily_stats，然后删除已聚合记录
pub async fn aggregate_past_days(db: &DatabaseConnection) {
    let today_start = today_utc_start_ts();

    let old_visits = match visit_log::Entity::find()
        .filter(visit_log::Column::VisitedAt.lt(today_start))
        .all(db)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            error!(target: "visitor_stats", error = %e, "查询历史 visit_log 失败");
            return;
        }
    };

    if old_visits.is_empty() {
        return;
    }

    // 在 Rust 中按日期分组计数，避免数据库方言差异
    let mut date_counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for visit in &old_visits {
        *date_counts.entry(ts_to_date_str(visit.visited_at)).or_insert(0) += 1;
    }

    // 逐日 upsert 到 visit_daily_stats
    for (date, cnt) in date_counts {
        match visit_daily_stats::Entity::find_by_id(&date).one(db).await {
            Ok(Some(model)) => {
                let new_count = model.total_count + cnt;
                let mut active: visit_daily_stats::ActiveModel = model.into();
                active.total_count = Set(new_count);
                if let Err(e) = active.update(db).await {
                    error!(target: "visitor_stats", error = %e, date = %date, "更新 daily_stats 失败");
                }
            }
            Ok(None) => {
                let new_stat = visit_daily_stats::ActiveModel {
                    date: Set(date.clone()),
                    total_count: Set(cnt),
                };
                if let Err(e) = new_stat.insert(db).await {
                    error!(target: "visitor_stats", error = %e, date = %date, "插入 daily_stats 失败");
                }
            }
            Err(e) => {
                error!(target: "visitor_stats", error = %e, date = %date, "查询 daily_stats 失败");
            }
        }
    }

    // 删除已聚合的 visit_log 记录
    if let Err(e) = visit_log::Entity::delete_many()
        .filter(visit_log::Column::VisitedAt.lt(today_start))
        .exec(db)
        .await
    {
        error!(target: "visitor_stats", error = %e, "删除历史 visit_log 失败");
    }
}

fn build_error(
    status: StatusCode,
    message: &str,
) -> axum::http::Response<jsonrpsee::server::HttpBody> {
    axum::http::Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(jsonrpsee::server::HttpBody::from(message.to_owned()))
        .expect("构建错误响应失败")
}
