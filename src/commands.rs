use docbox_management::{
    database::sqlx::types::Uuid,
    tenant::{
        create_tenant::CreateTenantConfig, delete_tenant::DeleteTenantOptions,
        migrate_tenants::MigrateTenantsConfig, migrate_tenants_search::MigrateTenantsSearchConfig,
        migrate_tenants_storage::MigrateTenantsStorageConfig,
    },
};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(tag = "command", content = "payload")]
pub enum ManagementCommand {
    /// Create and initialize the root database
    CreateRoot,
    /// Check the root database is initialized
    CheckRoot,
    /// Create a new tenant
    CreateTenant(CreateTenantConfig),
    /// Get a specific tenant
    GetTenant(GetTenantCommand),
    /// Delete a tenant
    DeleteTenant(DeleteTenantCommand),
    /// Get a list of tenants
    GetTenants(GetTenantsCommand),
    /// Set the allowed CORS origins for a tenant
    SetTenantAllowedCorsOrigins(SetTenantAllowedCorsOriginsCommand),
    /// Apply database migrations for a collection of tenants
    Migrate(MigrateTenantsConfig),
    /// Apply root migrations
    MigrateRoot,
    /// Apply search migrations for a collection of tenants
    MigrateSearch(MigrateTenantsSearchConfig),
    /// Apply storage migrations for a collection of tenants
    MigrateStorage(MigrateTenantsStorageConfig),
    /// Migrate a tenant from secrets based DB authentication to IAM authentication
    MigrateIAM(MigrateTenantIamCommand),
    /// Get pending migrations for a tenant
    GetTenantPendingMigrations(GetTenantPendingMigrationsCommand),
    /// Get pending search migrations for a tenant
    GetTenantPendingSearchMigrations(GetTenantPendingMigrationsCommand),
    /// Get pending storage migrations for a tenant
    GetTenantPendingStorageMigrations(GetTenantPendingMigrationsCommand),
}

#[derive(Debug, Serialize)]
pub struct GetTenantCommand {
    pub env: String,
    pub tenant_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct GetTenantPendingMigrationsCommand {
    pub env: String,
    pub tenant_id: Uuid,
}
#[derive(Debug, Serialize)]
pub struct DeleteTenantCommand {
    pub env: String,
    pub tenant_id: Uuid,
    pub options: DeleteTenantOptions,
}

#[derive(Debug, Serialize)]
pub struct SetTenantAllowedCorsOriginsCommand {
    pub env: String,
    pub tenant_id: Uuid,
    pub origins: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GetTenantsCommand {
    pub env: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MigrateTenantIamCommand {
    pub env: String,
    pub tenant_id: Option<Uuid>,
}
