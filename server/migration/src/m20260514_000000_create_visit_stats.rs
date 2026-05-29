use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 创建 visit_log 表：每次访问一行，保留当天数据
        manager
            .create_table(
                Table::create()
                    .table(VisitLog::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(VisitLog::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(VisitLog::Ip).string().not_null())
                    .col(ColumnDef::new(VisitLog::VisitedAt).big_integer().not_null())
                    .to_owned(),
            )
            .await?;

        // 在 visited_at 上建索引，加速按时间范围查询
        manager
            .create_index(
                Index::create()
                    .name("idx-visit-log-visited-at")
                    .table(VisitLog::Table)
                    .col(VisitLog::VisitedAt)
                    .to_owned(),
            )
            .await?;

        // 创建 visit_daily_stats 表：每天一行，存聚合后的访问量
        manager
            .create_table(
                Table::create()
                    .table(VisitDailyStats::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(VisitDailyStats::Date)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(VisitDailyStats::TotalCount)
                            .big_integer()
                            .not_null()
                            .default(0i64),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(VisitLog::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(VisitDailyStats::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum VisitLog {
    #[sea_orm(iden = "visit_log")]
    Table,
    Id,
    Ip,
    VisitedAt,
}

#[derive(DeriveIden)]
enum VisitDailyStats {
    #[sea_orm(iden = "visit_daily_stats")]
    Table,
    Date,
    TotalCount,
}
