//! `SeaORM` Entity，访问日志表，记录每次访问的 IP 和时间戳

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "visit_log")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub ip: String,
    /// Unix 时间戳（秒）
    pub visited_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
