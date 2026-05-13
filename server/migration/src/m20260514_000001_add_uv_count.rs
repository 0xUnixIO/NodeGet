use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(VisitDailyStats::Table)
                    .add_column(
                        ColumnDef::new(VisitDailyStats::UvCount)
                            .big_integer()
                            .not_null()
                            .default(0i64),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(VisitDailyStats::Table)
                    .drop_column(VisitDailyStats::UvCount)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum VisitDailyStats {
    #[sea_orm(iden = "visit_daily_stats")]
    Table,
    UvCount,
}
