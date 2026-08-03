use clap::{Parser, Subcommand, ValueEnum};
use comfy_table::{Cell, Table, modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL};
use docbox_management_interface::{
    CreateTenantInput, DeleteTenantInput, DeleteTenantOptions, DocboxManagementInterface,
    FailedTenantMigration, GetTenantInput, GetTenantsInput, MigrateTenantIAMInput,
    MigrateTenantInput, RemoteDocboxManagementInterface, SetTenantAllowedCorsOriginsInput,
};
use eyre::{Context, ContextCompat};
use serde_json::json;
use std::path::PathBuf;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

use crate::{
    aws_config::{aws_config, aws_config_with_profile},
    lambda_transport::{FunctionConfig, LambdaManagementTransport},
};

mod aws_config;
mod lambda_transport;

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

/// Which service the migration is for
#[derive(Debug, Clone, ValueEnum)]
pub enum TenantMigrationService {
    Database,
    Search,
    Storage,
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
        tenant_id: Uuid,
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
        tenant_id: Uuid,
    },

    /// Run a migration
    Migrate {
        // Environment to target
        #[arg(short, long)]
        env: Option<String>,
        /// Specific tenant to run against
        #[arg(short, long)]
        tenant_id: Option<Uuid>,
        /// Whether to ignore failures and continue
        #[arg(short, long)]
        skip_failed: bool,
        /// Specific service to target
        #[arg(short, long)]
        service: Option<TenantMigrationService>,
    },

    /// Run a root migration
    MigrateRoot,

    /// Set the allowed CORS origins for a tenant
    /// (Overrides existing CORS configuration)
    SetAllowedStorageCorsOrigins {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// ID of the tenant to target
        #[arg(short, long)]
        tenant_id: Uuid,
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
        tenant_id: Option<Uuid>,
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

    let transport = LambdaManagementTransport { client, config };
    let interface = RemoteDocboxManagementInterface::new(transport);

    match args.command {
        Commands::CreateRoot => {
            interface.create_root().await?;

            match args.format {
                OutputFormat::Human => {
                    println!("successfully created root");
                }
                OutputFormat::Json => {
                    println!("{{}}");
                }
            }

            Ok(())
        }

        Commands::CheckRoot => {
            let result = interface.check_root().await?;
            match args.format {
                OutputFormat::Human => {
                    if result.initialized {
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

            let tenant_config: CreateTenantInput =
                serde_json::from_str(&data).context("failed to parse tenant config")?;

            let result = interface.create_tenant(tenant_config).await?;
            let tenant = result.tenant;

            tracing::info!(?tenant, "tenant created successfully");

            match args.format {
                OutputFormat::Human => {
                    println!("tenant created successfully");

                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env"])
                        .add_row(vec![
                            Cell::new(tenant.id),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                        ]);

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
            let result = interface
                .delete_tenant(DeleteTenantInput {
                    env,
                    tenant_id,
                    options: DeleteTenantOptions {
                        delete_contents: delete_contents.unwrap_or_default(),
                        delete_database: delete_database.unwrap_or_default(),
                        delete_search: delete_search.unwrap_or_default(),
                        delete_storage: delete_storage.unwrap_or_default(),
                        permanently_delete_secret: permanently_delete_secret.unwrap_or_default(),
                    },
                })
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
            let result = interface.get_tenants(GetTenantsInput { env }).await?;

            match args.format {
                OutputFormat::Human => {
                    let tenants = result.tenants;
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env"]);

                    for tenant in tenants {
                        table.add_row(vec![
                            Cell::new(tenant.id),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                        ]);
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
            let result = interface
                .get_tenant(GetTenantInput { env, tenant_id })
                .await?;

            let tenant = result.tenant.context("tenant not found")?;

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

                    table.add_row(vec![Cell::new("ID"), Cell::new(tenant.id.to_string())]);
                    table.add_row(vec![Cell::new("Name"), Cell::new(tenant.name)]);
                    table.add_row(vec![Cell::new("Env"), Cell::new(tenant.env)]);
                    table.add_row(vec![Cell::new("DB Name"), Cell::new(tenant.db_name)]);
                    table.add_row(vec![
                        Cell::new("DB Secret Name"),
                        Cell::new(if let Some(value) = tenant.db_secret_name {
                            value
                        } else {
                            "-- None --".to_string()
                        }),
                    ]);
                    table.add_row(vec![
                        Cell::new("DB IAM User Name"),
                        Cell::new(if let Some(value) = tenant.db_iam_user_name {
                            value
                        } else {
                            "-- None --".to_string()
                        }),
                    ]);
                    table.add_row(vec![
                        Cell::new("Storage Bucket Name"),
                        Cell::new(tenant.s3_name),
                    ]);
                    table.add_row(vec![
                        Cell::new("Search Index Name"),
                        Cell::new(tenant.os_index_name),
                    ]);
                    table.add_row(vec![
                        Cell::new("Event Queue URL"),
                        Cell::new(if let Some(value) = tenant.event_queue_url {
                            value
                        } else {
                            "-- None --".to_string()
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
            service,
        } => {
            let result = interface
                .migrate_tenant(MigrateTenantInput {
                    env,
                    tenant_id,
                    skip_failed,
                    name: None,
                    service: service.map(|service| match service {
                        TenantMigrationService::Database => {
                            docbox_management_interface::TenantMigrationService::Database
                        }
                        TenantMigrationService::Search => {
                            docbox_management_interface::TenantMigrationService::Search
                        }
                        TenantMigrationService::Storage => {
                            docbox_management_interface::TenantMigrationService::Storage
                        }
                    }),
                })
                .await?;

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env", "Outcome"]);

                    for tenant in result.applied_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.id),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new("Success"),
                        ]);
                    }
                    for FailedTenantMigration { error, target } in result.failed_tenants {
                        table.add_row(vec![
                            Cell::new(target.id),
                            Cell::new(target.name),
                            Cell::new(target.env),
                            Cell::new(format!("Failed: {error}")),
                        ]);
                    }

                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                }
            }

            Ok(())
        }

        Commands::MigrateRoot => {
            interface.migrate_root().await?;

            match args.format {
                OutputFormat::Human => {
                    println!("Migrations applied")
                }
                OutputFormat::Json => {
                    println!("{{}}");
                }
            }

            Ok(())
        }

        Commands::SetAllowedStorageCorsOrigins {
            env,
            tenant_id,
            origin: origins,
        } => {
            interface
                .set_tenant_allowed_cors_origins(SetTenantAllowedCorsOriginsInput {
                    env,
                    tenant_id,
                    origins,
                })
                .await?;

            match args.format {
                OutputFormat::Human => {
                    println!("updated tenant allowed origins")
                }
                OutputFormat::Json => {
                    println!("{{}}");
                }
            }

            Ok(())
        }

        Commands::MigrateTenantIam { env, tenant_id } => {
            let result = interface
                .migrate_tenant_iam(MigrateTenantIAMInput { env, tenant_id })
                .await?;

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env", "Outcome"]);

                    for tenant in result.applied_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.id),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
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
