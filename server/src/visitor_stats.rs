use axum::http::StatusCode;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    Set,
};
use serde::Serialize;
use tracing::error;

use crate::entity::{visit_daily_stats, visit_log};

#[derive(Serialize)]
struct DailyPoint {
    date: String,
    pv: u64,
    uv: u64,
}

#[derive(Serialize)]
struct VisitorStatsResponse {
    today_rank: u64,
    today_pv: u64,
    today_uv: u64,
    all_time_pv: u64,
    all_time_uv: u64,
    yesterday_pv: u64,
    yesterday_uv: u64,
    /// 最近 14 天（含今日）的每日数据，按日期升序
    history: Vec<DailyPoint>,
}

/// X-Real-IP → X-Forwarded-For → ConnectInfo 三级降级，并验证 IP 格式防止头部伪造污染数据
fn extract_ip(req: &axum::extract::Request) -> String {
    if let Some(ip) = req
        .headers()
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| s.parse::<std::net::IpAddr>().is_ok())
    {
        return ip;
    }
    if let Some(forwarded) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        let first = forwarded.split(',').next().unwrap_or("").trim();
        if first.parse::<std::net::IpAddr>().is_ok() {
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
    let today_start = today_utc_start_ts();

    // 5分钟内同 IP 去重：超过 5 分钟才算新 PV
    let five_min_ago = now_ts - 300;
    let recent_count = visit_log::Entity::find()
        .filter(visit_log::Column::Ip.eq(&ip))
        .filter(visit_log::Column::VisitedAt.gte(five_min_ago))
        .count(db)
        .await
        .unwrap_or(0);

    if recent_count == 0 {
        let new_visit = visit_log::ActiveModel {
            ip: Set(ip),
            visited_at: Set(now_ts),
            ..Default::default()
        };
        if let Err(e) = new_visit.insert(db).await {
            error!(target: "visitor_stats", error = %e, "插入 visit_log 失败");
            return build_error(StatusCode::INTERNAL_SERVER_ERROR, "记录访问失败");
        }
    }

    // 今日所有记录，用于计算 PV / UV
    let today_records = match visit_log::Entity::find()
        .filter(visit_log::Column::VisitedAt.gte(today_start))
        .all(db)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            error!(target: "visitor_stats", error = %e, "查询今日 visit_log 失败");
            return build_error(StatusCode::INTERNAL_SERVER_ERROR, "查询失败");
        }
    };

    let today_pv = today_records.len() as u64;
    let today_uv = today_records
        .iter()
        .map(|r| r.ip.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len() as u64;
    let today_rank = today_pv;

    // 历史聚合数据（今天之前）
    let all_daily = visit_daily_stats::Entity::find()
        .all(db)
        .await
        .unwrap_or_default();
    let daily_pv_sum: i64 = all_daily.iter().map(|m| m.total_count).sum();
    let daily_uv_sum: i64 = all_daily.iter().map(|m| m.uv_count).sum();

    // 全部 visit_log 记录数（聚合未运行时可能含今天之前的数据）
    let log_total = visit_log::Entity::find().count(db).await.unwrap_or(0) as i64;
    let all_time_pv = (daily_pv_sum + log_total).max(0) as u64;
    let all_time_uv = (daily_uv_sum + today_uv as i64).max(0) as u64;

    // 昨日统计
    let yesterday_start = today_start - 86400;
    let yesterday_date = ts_to_date_str(yesterday_start);
    let (yesterday_pv, yesterday_uv) =
        match visit_daily_stats::Entity::find_by_id(&yesterday_date)
            .one(db)
            .await
            .unwrap_or(None)
        {
            Some(m) => (m.total_count.max(0) as u64, m.uv_count.max(0) as u64),
            None => {
                // 聚合任务尚未运行时从 visit_log 实时计算
                let yesterday_records = visit_log::Entity::find()
                    .filter(visit_log::Column::VisitedAt.gte(yesterday_start))
                    .filter(visit_log::Column::VisitedAt.lt(today_start))
                    .all(db)
                    .await
                    .unwrap_or_default();
                let pv = yesterday_records.len() as u64;
                let uv = yesterday_records
                    .iter()
                    .map(|r| r.ip.as_str())
                    .collect::<std::collections::HashSet<_>>()
                    .len() as u64;
                (pv, uv)
            }
        };

    // 最近 14 天趋势：从 all_daily 中取，按日期升序排列，最后追加今日
    let today_date = ts_to_date_str(today_start);
    let mut history: Vec<DailyPoint> = {
        let mut days: Vec<_> = all_daily
            .iter()
            .filter(|m| m.date != today_date)
            .map(|m| DailyPoint {
                date: m.date.clone(),
                pv: m.total_count.max(0) as u64,
                uv: m.uv_count.max(0) as u64,
            })
            .collect();
        days.sort_by(|a, b| a.date.cmp(&b.date));
        // 保留最近 13 天历史，加上今日共 14 天
        let len = days.len();
        if len > 13 {
            days.drain(..len - 13);
        }
        days
    };
    history.push(DailyPoint {
        date: today_date,
        pv: today_pv,
        uv: today_uv,
    });

    let resp = VisitorStatsResponse {
        today_rank,
        today_pv,
        today_uv,
        all_time_pv,
        all_time_uv,
        yesterday_pv,
        yesterday_uv,
        history,
    };

    match serde_json::to_string(&resp) {
        Ok(body) => axum::http::Response::builder()
            .status(StatusCode::OK)
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/json; charset=utf-8",
            )
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

    // 按日期分组：(pv_count, ip_set)
    let mut date_counts: std::collections::HashMap<
        String,
        (i64, std::collections::HashSet<String>),
    > = std::collections::HashMap::new();
    for visit in &old_visits {
        let entry = date_counts
            .entry(ts_to_date_str(visit.visited_at))
            .or_default();
        entry.0 += 1;
        entry.1.insert(visit.ip.clone());
    }

    // 逐日 upsert
    for (date, (pv_cnt, uv_set)) in date_counts {
        let uv_cnt = uv_set.len() as i64;
        match visit_daily_stats::Entity::find_by_id(&date).one(db).await {
            Ok(Some(model)) => {
                let new_total = model.total_count + pv_cnt;
                let new_uv = model.uv_count + uv_cnt;
                let mut active: visit_daily_stats::ActiveModel = model.into();
                active.total_count = Set(new_total);
                active.uv_count = Set(new_uv);
                if let Err(e) = active.update(db).await {
                    error!(target: "visitor_stats", error = %e, date = %date, "更新 daily_stats 失败");
                }
            }
            Ok(None) => {
                let new_stat = visit_daily_stats::ActiveModel {
                    date: Set(date.clone()),
                    total_count: Set(pv_cnt),
                    uv_count: Set(uv_cnt),
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

    // 删除已聚合的记录
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
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )
        .header(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(jsonrpsee::server::HttpBody::from(message.to_owned()))
        .expect("构建错误响应失败")
}
