pub use sea_orm_migration::prelude::*;

mod m20260728000000_initial_schema;
mod m20260729000000_build_logs;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260728000000_initial_schema::Migration),
            Box::new(m20260729000000_build_logs::Migration),
        ]
    }
}
