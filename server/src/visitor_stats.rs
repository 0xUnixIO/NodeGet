//! 访客统计模块
//!
//! 提供 GET /nodeget/visitor-stats 端点，记录每次访问并返回统计数据。
//! 后台任务每天 UTC 零点将 visit_log 中历史数据聚合到 visit_daily_stats。

use axum::http::StatusCode;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, Set, Statement};
use serde::Serialize;
use tracing::error;

use crate::entity::visit_log;

/// 访客统计响应结构
#[derive(Serialize)]
struct VisitorStatsResponse {
    /// 今日排名（等同于今日总访问量）
    today_rank: i64,
    /// 今日总访问量
    today_total: i64,
    /// 全部时间总访问量
    all_time_total: i64,
    /// 昨日总访问量
    yesterday_total: i64,
}

/// 从请求头中提取访客 IP
/// 优先级：X-Real-IP → X-Forwarded-For（取第一个）→ ConnectInfo
fn extract_ip(req: &axum::extract::Request) -> String {
    // 优先取 X-Real-IP
    if let Some(real_ip) = req
        .headers()
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
    {
        return real_ip;
    }

    // 其次取 X-Forwarded-For 的第一个地址
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

    // 最后从 ConnectInfo 取
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map_or_else(|| "127.0.0.1".to_owned(), |info| info.0.ip().to_string())
}

/// 返回今天 UTC 零点的 Unix 时间戳（秒）
fn today_utc_start_ts() -> i64 {
    let now = chrono::Utc::now();
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp()
}

/// 将 Unix 时间戳转为 YYYY-MM-DD 字符串（UTC）
fn ts_to_date_str(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_owned())
}

/// 执行 raw SQL COUNT 查询，返回第一列第一行的 i64 值，出错返回 0
async fn query_count(db: &DatabaseConnection, sql: &str) -> i64 {
    match db
        .query_one(&Statement::from_string(db.get_database_backend(), sql.to_owned()))
        .await
    {
        Ok(Some(row)) => row.try_get_by_index::<i64>(0).unwrap_or(0),
        Ok(None) => 0,
        Err(e) => {
            error!(target: "visitor_stats", error = %e, sql = %sql, "query_count 执行失败");
            0
        }
    }
}

/// 访客统计 HTTP handler
///
/// - 记录本次访问到 visit_log
/// - 返回 JSON 统计数据，并带 CORS 头
pub async fn handler(
    req: axum::extract::Request,
) -> axum::http::Response<jsonrpsee::server::HttpBody> {
    // 处理 OPTIONS 预检请求
    if req.method() == axum::http::Method::OPTIONS {
        return axum::http::Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(
                axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
                "GET, OPTIONS",
            )
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

    // 用 ActiveModel 插入，避免 SQL 注入（IP 来自可伪造的请求头）
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

    // 今日访问量
    let today_total = query_count(
        db,
        &format!(
            "SELECT COUNT(*) FROM visit_log WHERE visited_at >= {today_start}"
        ),
    )
    .await;

    // 全部时间总量 = visit_daily_stats 历史总和 + visit_log 全部记录
    let daily_sum = query_count(
        db,
        "SELECT COALESCE(SUM(total_count), 0) FROM visit_daily_stats",
    )
    .await;
    let log_total = query_count(db, "SELECT COUNT(*) FROM visit_log").await;
    let all_time_total = daily_sum + log_total;

    // 昨日访问量：先查 visit_daily_stats，找不到再查 visit_log
    let yesterday_date = ts_to_date_str(yesterday_start);
    let yesterday_in_daily = query_count(
        db,
        &format!(
            "SELECT COALESCE(SUM(total_count), 0) FROM visit_daily_stats WHERE date = '{yesterday_date}'"
        ),
    )
    .await;
    let yesterday_total = if yesterday_in_daily > 0 {
        yesterday_in_daily
    } else {
        query_count(
            db,
            &format!(
                "SELECT COUNT(*) FROM visit_log WHERE visited_at >= {yesterday_start} AND visited_at < {today_start}"
            ),
        )
        .await
    };

    let resp = VisitorStatsResponse {
        today_rank: today_total,
        today_total,
        all_time_total,
        yesterday_total,
    };

    let body = match serde_json::to_string(&resp) {
        Ok(s) => s,
        Err(e) => {
            error!(target: "visitor_stats", error = %e, "序列化响应失败");
            return build_error(StatusCode::INTERNAL_SERVER_ERROR, "序列化失败");
        }
    };

    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(jsonrpsee::server::HttpBody::from(body))
        .expect("构建响应失败")
}

/// 将 visit_log 中今天之前的历史数据聚合到 visit_daily_stats，然后删除已聚合记录
pub async fn aggregate_past_days(db: &DatabaseConnection) {
    let today_start = today_utc_start_ts();
    let backend = db.get_database_backend();

    // 按日期分组统计历史访问量
    let group_sql = match backend {
        DatabaseBackend::Sqlite => format!(
            "SELECT strftime('%Y-%m-%d', datetime(visited_at, 'unixepoch')) as date, COUNT(*) as cnt \
             FROM visit_log WHERE visited_at < {today_start} GROUP BY date"
        ),
        DatabaseBackend::Postgres => format!(
            "SELECT TO_CHAR(TO_TIMESTAMP(visited_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD') as date, COUNT(*) as cnt \
             FROM visit_log WHERE visited_at < {today_start} GROUP BY date"
        ),
        _ => {
            error!(target: "visitor_stats", "不支持的数据库后端，跳过聚合");
            return;
        }
    };

    let rows = match db
        .query_all(&Statement::from_string(backend, group_sql))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            error!(target: "visitor_stats", error = %e, "查询历史 visit_log 失败");
            return;
        }
    };

    if rows.is_empty() {
        return;
    }

    // 逐行 upsert 到 visit_daily_stats
    for row in &rows {
        let date: String = match row.try_get_by_index(0) {
            Ok(d) => d,
            Err(e) => {
                error!(target: "visitor_stats", error = %e, "读取日期列失败");
                continue;
            }
        };
        let cnt: i64 = match row.try_get_by_index(1) {
            Ok(c) => c,
            Err(e) => {
                error!(target: "visitor_stats", error = %e, "读取计数列失败");
                continue;
            }
        };

        let upsert_sql = match backend {
            DatabaseBackend::Sqlite => format!(
                "INSERT INTO visit_daily_stats (date, total_count) VALUES ('{date}', {cnt}) \
                 ON CONFLICT(date) DO UPDATE SET total_count = total_count + excluded.total_count"
            ),
            DatabaseBackend::Postgres => format!(
                "INSERT INTO visit_daily_stats (date, total_count) VALUES ('{date}', {cnt}) \
                 ON CONFLICT (date) DO UPDATE SET total_count = visit_daily_stats.total_count + EXCLUDED.total_count"
            ),
            _ => continue,
        };

        if let Err(e) = db
            .execute(&Statement::from_string(backend, upsert_sql))
            .await
        {
            error!(target: "visitor_stats", error = %e, date = %date, "upsert visit_daily_stats 失败");
        }
    }

    // 删除已聚合的 visit_log 历史记录
    let delete_sql = format!(
        "DELETE FROM visit_log WHERE visited_at < {today_start}"
    );
    if let Err(e) = db
        .execute(&Statement::from_string(backend, delete_sql))
        .await
    {
        error!(target: "visitor_stats", error = %e, "删除历史 visit_log 失败");
    }
}

/// 构建带 CORS 头的错误响应
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
