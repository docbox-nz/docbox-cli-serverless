use aws_sdk_lambda::primitives::Blob;
use clap::{Parser, Subcommand, ValueEnum};
use comfy_table::{Cell, Table, modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL};
use docbox_management::{
    core::aws::{aws_config, aws_config_with_profile},
    database::models::tenant::TenantId,
    tenant::{
        MigrateTenantsOutcome, create_tenant::CreateTenantConfig,
        delete_tenant::DeleteTenantOptions, migrate_tenants::MigrateTenantsConfig,
        migrate_tenants_search::MigrateTenantsSearchConfig,
        migrate_tenants_storage::MigrateTenantsStorageConfig,
    },
};
use eyre::{Context, ContextCompat, eyre};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::path::PathBuf;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::commands::{
    DeleteTenantCommand, GetTenantCommand, GetTenantsCommand, ManagementCommand,
    MigrateTenantIamCommand, SetTenantAllowedCorsOriginsCommand,
};

mod commands;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    pub command: Commands,

    /// Optional AWS profile to use
    #[arg(long)]
    pub aws_profile: Option<String>,

    /// Name of the management function
    #[arg(long)]
    pub function_name: String,
    /// Specify a version or alias to invoke a published version of the function.
    #[arg(long)]
    pub function_qualifier: Option<String>,
    /// The identifier of the tenant in a multi-tenant Lambda function.
    #[arg(long)]
    pub function_tenant_id: Option<String>,

    /// Output format for how to display the resulting data
    #[arg(short, long, default_value = "human")]
    pub format: OutputFormat,
}

#[derive(ValueEnum, Clone)]
pub enum OutputFormat {
    /// Provide output in human readable format
    Human,

    /// Provide output in machine readable JSON format
    Json,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize the root docbox database
    CreateRoot,

    /// Check if the root docbox database is initialized
    CheckRoot,

    /// Create a new tenant
    CreateTenant {
        /// File containing the tenant configuration details
        #[arg(short, long)]
        file: PathBuf,
    },

    /// Delete a tenant
    DeleteTenant {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// Specific tenant to delete
        #[arg(short, long)]
        tenant_id: TenantId,
        /// Whether to delete data stored within the tenant
        #[arg(short = 'c', long)]
        delete_contents: Option<bool>,
        /// Whether to delete the tenant storage bucket itself (Requires "delete-contents")
        #[arg(short = 'd', long)]
        delete_database: Option<bool>,
        /// Whether to delete the tenant search index itself (Requires "delete-contents")
        #[arg(short = 'i', long)]
        delete_search: Option<bool>,
        /// Whether to delete the tenant database itself (Requires "delete-contents")
        #[arg(short = 's', long)]
        delete_storage: Option<bool>,
        /// Whether when using AWS secrets manager to immediately delete the secret
        /// or to allow it to be recoverable for a short period of time. (Requires "delete-contents")
        ///
        /// Note: If the secret is not immediately deleted a new tenant will not be
        /// able to make use of this secret name until the 30day recovery window
        /// has ended.
        #[arg(short = 'p', long)]
        permanently_delete_secret: Option<bool>,
    },

    /// Get all tenants
    GetTenants {
        // Environment to filter to
        #[arg(short, long)]
        env: Option<String>,
    },

    /// Get a tenant
    GetTenant {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// Specific tenant to delete
        #[arg(short, long)]
        tenant_id: TenantId,
    },

    /// Run a migration
    Migrate {
        // Environment to target
        #[arg(short, long)]
        env: Option<String>,
        /// Specific tenant to run against
        #[arg(short, long)]
        tenant_id: Option<TenantId>,
        #[arg(short, long)]
        skip_failed: bool,
    },

    /// Run a root migration
    MigrateRoot,

    /// Run a search migration
    MigrateSearch {
        // Environment to target
        #[arg(short, long)]
        env: Option<String>,
        /// Optional Name of the migration
        #[arg(short, long)]
        name: Option<String>,
        /// Specific tenant to run against
        #[arg(short, long)]
        tenant_id: Option<TenantId>,
        /// Skip failed migrations
        #[arg(short, long)]
        skip_failed: bool,
    },

    /// Run a storage migration
    MigrateStorage {
        // Environment to target
        #[arg(short, long)]
        env: Option<String>,
        /// Optional Name of the migration
        #[arg(short, long)]
        name: Option<String>,
        /// Specific tenant to run against
        #[arg(short, long)]
        tenant_id: Option<TenantId>,
        /// Skip failed migrations
        #[arg(short, long)]
        skip_failed: bool,
    },

    /// Set the allowed CORS origins for a tenant
    /// (Overrides existing CORS configuration)
    SetAllowedStorageCorsOrigins {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// ID of the tenant to target
        #[arg(short, long)]
        tenant_id: TenantId,
        /// Allowed origins to set
        #[arg(short, long)]
        origin: Vec<String>,
    },

    /// Migrate tenants from secrets to IAM
    MigrateTenantIam {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// Specific tenant to run against
        #[arg(short, long)]
        tenant_id: Option<TenantId>,
    },
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let args = Args::parse();
    let format = args.format.clone();

    if let Err(error) = app(args).await {
        match format {
            OutputFormat::Human => {
                return Err(error);
            }
            OutputFormat::Json => {
                tracing::error!(?error, "error occurred");

                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "error": error.to_string()
                    }))?
                );

                return Err(error);
            }
        }
    }

    Ok(())
}

async fn app(args: Args) -> eyre::Result<()> {
    // Load environment variables
    _ = dotenvy::dotenv();

    // Setup colorful error logging
    color_eyre::install()?;

    let indicatif_layer = IndicatifLayer::new();

    tracing_subscriber::registry()
        .with(
            EnvFilter::from_default_env()
                // Provide logging from docbox by default
                .add_directive("docbox=info".parse()?)
                .add_directive("docbox_core=info".parse()?)
                .add_directive("docbox_database=info".parse()?)
                .add_directive("docbox_management=info".parse()?)
                .add_directive("docbox_search=info".parse()?)
                .add_directive("docbox_secrets=info".parse()?)
                .add_directive("docbox_storage=info".parse()?)
                //
                .add_directive("aws_sdk_secretsmanager=info".parse()?)
                .add_directive("aws_runtime=info".parse()?)
                .add_directive("aws_smithy_runtime=info".parse()?)
                .add_directive("hyper_util=info".parse()?),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_line_number(false)
                .with_target(false)
                .with_file(false)
                .with_writer(indicatif_layer.get_stderr_writer()),
        )
        .with(indicatif_layer)
        .init();

    let aws_config = match args.aws_profile {
        Some(profile) => aws_config_with_profile(profile).await,
        None => aws_config().await,
    };

    let client = aws_sdk_lambda::Client::new(&aws_config);
    let config = FunctionConfig {
        name: args.function_name,
        qualifier: args.function_qualifier,
        tenant_id: args.function_tenant_id,
    };

    match args.command {
        Commands::CreateRoot => {
            let result: serde_json::Value =
                invoke_management_command(&client, &config, ManagementCommand::CreateRoot).await?;

            match args.format {
                OutputFormat::Human => {
                    println!("successfully created root");
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                }
            }

            Ok(())
        }

        Commands::CheckRoot => {
            let result: serde_json::Value =
                invoke_management_command(&client, &config, ManagementCommand::CheckRoot).await?;

            match args.format {
                OutputFormat::Human => {
                    let is_initialized = result
                        .get("is_initialized")
                        .context("missing is_initialized")?
                        .as_bool()
                        .context("expected boolean")?;

                    if is_initialized {
                        println!("root is initialized");
                    } else {
                        println!("root is not initialized");
                    }
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                }
            }

            Ok(())
        }

        Commands::CreateTenant { file } => {
            let data = tokio::fs::read_to_string(file)
                .await
                .context("failed to read tenant file")?;

            let tenant_config: CreateTenantConfig =
                serde_json::from_str(&data).context("failed to parse tenant config")?;

            let tenant: serde_json::Value = invoke_management_command(
                &client,
                &config,
                ManagementCommand::CreateTenant(tenant_config),
            )
            .await?;

            tracing::info!(?tenant, "tenant created successfully");

            match args.format {
                OutputFormat::Human => {
                    println!("tenant created successfully");

                    let id = tenant
                        .get("id")
                        .context("tenant missing id")?
                        .as_str()
                        .context("id was not a string")?;

                    let name = tenant
                        .get("name")
                        .context("tenant missing id")?
                        .as_str()
                        .context("id was not a string")?;

                    let env = tenant
                        .get("env")
                        .context("tenant missing id")?
                        .as_str()
                        .context("id was not a string")?;

                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env"])
                        .add_row(vec![Cell::new(id), Cell::new(name), Cell::new(env)]);

                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&tenant)?);
                }
            }

            Ok(())
        }

        Commands::DeleteTenant {
            env,
            tenant_id,
            delete_contents,
            delete_database,
            delete_search,
            delete_storage,
            permanently_delete_secret,
        } => {
            let result: serde_json::Value = invoke_management_command(
                &client,
                &config,
                ManagementCommand::DeleteTenant(DeleteTenantCommand {
                    env,
                    tenant_id,
                    options: DeleteTenantOptions {
                        delete_contents: delete_contents.unwrap_or_default(),
                        delete_database: delete_database.unwrap_or_default(),
                        delete_search: delete_search.unwrap_or_default(),
                        delete_storage: delete_storage.unwrap_or_default(),
                        permanently_delete_secret: permanently_delete_secret.unwrap_or_default(),
                    },
                }),
            )
            .await?;

            match args.format {
                OutputFormat::Human => {
                    println!("deleted tenant")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                }
            }

            Ok(())
        }

        Commands::GetTenants { env } => {
            let result: serde_json::Value = invoke_management_command(
                &client,
                &config,
                ManagementCommand::GetTenants(GetTenantsCommand { env }),
            )
            .await?;

            match args.format {
                OutputFormat::Human => {
                    let tenants = result.as_array().context("expected tenants array")?;

                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env"]);

                    for tenant in tenants {
                        let id = tenant
                            .get("id")
                            .context("tenant missing id")?
                            .as_str()
                            .context("id was not a string")?;

                        let name = tenant
                            .get("name")
                            .context("tenant missing id")?
                            .as_str()
                            .context("id was not a string")?;

                        let env = tenant
                            .get("env")
                            .context("tenant missing id")?
                            .as_str()
                            .context("id was not a string")?;

                        table.add_row(vec![Cell::new(id), Cell::new(name), Cell::new(env)]);
                    }

                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                }
            }

            Ok(())
        }

        Commands::GetTenant { env, tenant_id } => {
            let tenant: serde_json::Value = invoke_management_command(
                &client,
                &config,
                ManagementCommand::GetTenant(GetTenantCommand { env, tenant_id }),
            )
            .await?;

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

                    let id = tenant
                        .get("id")
                        .context("tenant missing id")?
                        .as_str()
                        .context("id was not a string")?;

                    let name = tenant
                        .get("name")
                        .context("tenant missing id")?
                        .as_str()
                        .context("id was not a string")?;

                    let env = tenant
                        .get("env")
                        .context("tenant missing id")?
                        .as_str()
                        .context("id was not a string")?;

                    let db_name = tenant
                        .get("db_name")
                        .context("tenant missing db_name")?
                        .as_str()
                        .context("db_name was not a string")?;

                    let s3_name = tenant
                        .get("s3_name")
                        .context("tenant missing s3_name")?
                        .as_str()
                        .context("s3_name was not a string")?;

                    let os_index_name = tenant
                        .get("os_index_name")
                        .context("tenant missing os_index_name")?
                        .as_str()
                        .context("os_index_name was not a string")?;

                    let db_secret_name = tenant
                        .get("db_secret_name")
                        .and_then(|value| value.as_str());

                    let db_iam_user_name = tenant
                        .get("db_iam_user_name")
                        .and_then(|value| value.as_str());

                    let event_queue_url = tenant
                        .get("event_queue_url")
                        .and_then(|value| value.as_str());

                    table.add_row(vec![Cell::new("ID"), Cell::new(id.to_string())]);
                    table.add_row(vec![Cell::new("Name"), Cell::new(name)]);
                    table.add_row(vec![Cell::new("Env"), Cell::new(env)]);
                    table.add_row(vec![Cell::new("DB Name"), Cell::new(db_name)]);
                    table.add_row(vec![
                        Cell::new("DB Secret Name"),
                        Cell::new(if let Some(value) = db_secret_name {
                            format!("Some({value}")
                        } else {
                            "None".to_string()
                        }),
                    ]);
                    table.add_row(vec![
                        Cell::new("DB IAM User Name"),
                        Cell::new(if let Some(value) = db_iam_user_name {
                            format!("Some({value}")
                        } else {
                            "None".to_string()
                        }),
                    ]);
                    table.add_row(vec![Cell::new("Storage Bucket Name"), Cell::new(s3_name)]);
                    table.add_row(vec![
                        Cell::new("Search Index Name"),
                        Cell::new(os_index_name),
                    ]);
                    table.add_row(vec![
                        Cell::new("Event Queue URL"),
                        Cell::new(if let Some(value) = event_queue_url {
                            format!("Some({value}")
                        } else {
                            "None".to_string()
                        }),
                    ]);

                    println!("{table}");
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&tenant)?);
                }
            }

            Ok(())
        }

        Commands::Migrate {
            env,
            tenant_id,
            skip_failed,
        } => {
            let outcome: MigrateTenantsOutcome = invoke_management_command(
                &client,
                &config,
                ManagementCommand::Migrate(MigrateTenantsConfig {
                    env,
                    tenant_id,
                    skip_failed,
                    target_migration_name: None,
                }),
            )
            .await?;

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env", "Outcome"]);

                    for tenant in outcome.applied_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.tenant_id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new("Success"),
                        ]);
                    }
                    for (error, tenant) in outcome.failed_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.tenant_id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new(format!("Failed: {error}")),
                        ]);
                    }

                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&outcome)?);
                }
            }

            Ok(())
        }

        Commands::MigrateRoot => {
            let result: serde_json::Value =
                invoke_management_command(&client, &config, ManagementCommand::MigrateRoot).await?;

            match args.format {
                OutputFormat::Human => {
                    println!("Migrations applied")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                }
            }

            Ok(())
        }

        Commands::MigrateSearch {
            env,
            name,
            tenant_id,
            skip_failed,
        } => {
            let outcome: MigrateTenantsOutcome = invoke_management_command(
                &client,
                &config,
                ManagementCommand::MigrateSearch(MigrateTenantsSearchConfig {
                    env,
                    tenant_id,
                    skip_failed,
                    target_migration_name: name,
                }),
            )
            .await?;

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env", "Outcome"]);

                    for tenant in outcome.applied_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.tenant_id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new("Success"),
                        ]);
                    }
                    for (error, tenant) in outcome.failed_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.tenant_id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new(format!("Failed: {error}")),
                        ]);
                    }

                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&outcome)?);
                }
            }

            Ok(())
        }

        Commands::MigrateStorage {
            env,
            name,
            tenant_id,
            skip_failed,
        } => {
            let outcome: MigrateTenantsOutcome = invoke_management_command(
                &client,
                &config,
                ManagementCommand::MigrateStorage(MigrateTenantsStorageConfig {
                    env,
                    tenant_id,
                    skip_failed,
                    target_migration_name: name,
                }),
            )
            .await?;

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env", "Outcome"]);

                    for tenant in outcome.applied_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.tenant_id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new("Success"),
                        ]);
                    }
                    for (error, tenant) in outcome.failed_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.tenant_id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new(format!("Failed: {error}")),
                        ]);
                    }

                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&outcome)?);
                }
            }

            Ok(())
        }

        Commands::SetAllowedStorageCorsOrigins {
            env,
            tenant_id,
            origin,
        } => {
            let result: serde_json::Value = invoke_management_command(
                &client,
                &config,
                ManagementCommand::SetTenantAllowedCorsOrigins(
                    SetTenantAllowedCorsOriginsCommand {
                        env,
                        tenant_id,
                        origins: origin,
                    },
                ),
            )
            .await?;

            match args.format {
                OutputFormat::Human => {
                    println!("updated tenant allowed origins")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                }
            }

            Ok(())
        }

        Commands::MigrateTenantIam { env, tenant_id } => {
            let result: Vec<serde_json::Value> = invoke_management_command(
                &client,
                &config,
                ManagementCommand::MigrateIAM(MigrateTenantIamCommand { env, tenant_id }),
            )
            .await?;

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env", "Outcome"]);

                    for tenant in result {
                        let id = tenant
                            .get("id")
                            .context("tenant missing id")?
                            .as_str()
                            .context("id was not a string")?;

                        let name = tenant
                            .get("name")
                            .context("tenant missing id")?
                            .as_str()
                            .context("id was not a string")?;

                        let env = tenant
                            .get("env")
                            .context("tenant missing id")?
                            .as_str()
                            .context("id was not a string")?;

                        table.add_row(vec![
                            Cell::new(id),
                            Cell::new(name),
                            Cell::new(env),
                            Cell::new("Success"),
                        ]);
                    }

                    println!("migrated tenants to IAM based authentication");
                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                }
            }

            Ok(())
        }
    }
}

struct FunctionConfig {
    name: String,
    qualifier: Option<String>,
    tenant_id: Option<String>,
}

async fn invoke_management_command<R>(
    client: &aws_sdk_lambda::Client,
    config: &FunctionConfig,
    command: ManagementCommand,
) -> eyre::Result<R>
where
    R: DeserializeOwned,
{
    let message = serde_json::to_string(&command).context("failed to serialize request")?;

    let output = client
        .invoke()
        .payload(Blob::new(message))
        .function_name(&config.name)
        .set_qualifier(config.qualifier.clone())
        .set_tenant_id(config.tenant_id.clone())
        .send()
        .await?;

    if let Some(error) = output.function_error {
        return Err(eyre!("management error: {error}"));
    }

    let payload = output.payload().context("missing response payload")?;
    let result: R = serde_json::from_slice(payload.as_ref()).context("failed to parse response")?;
    Ok(result)
}
