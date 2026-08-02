pub use sea_orm_migration::prelude::*;

mod m20260728000000_initial_schema;
mod m20260729000000_build_logs;
mod m20260730000000_build_retention_limit;
mod m20260801000000_build_approval_evidence;
mod m20260802000000_build_capture_plan;
mod m20260803000000_build_captured_names;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260728000000_initial_schema::Migration),
            Box::new(m20260729000000_build_logs::Migration),
            Box::new(m20260730000000_build_retention_limit::Migration),
            Box::new(m20260801000000_build_approval_evidence::Migration),
            Box::new(m20260802000000_build_capture_plan::Migration),
            Box::new(m20260803000000_build_captured_names::Migration),
        ]
    }
}
