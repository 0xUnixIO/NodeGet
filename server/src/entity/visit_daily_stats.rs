//! `SeaORM` Entity，每日访问统计表，聚合后的历史数据每天一行

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "visit_daily_stats")]
pub struct Model {
    /// 日期字符串，格式 YYYY-MM-DD，作为主键
    #[sea_orm(primary_key, auto_increment = false)]
    pub date: String,
    pub total_count: i64,
    pub uv_count: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
