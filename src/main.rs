mod api;
mod addons;
mod addon_capabilities;
mod auth;
mod backup_api;
mod db;
mod display;
mod discovery;
mod fonts;
mod github_update;
mod middleware;
mod models;
mod news;
mod observability;
mod postgres;
mod rate_limit;
mod schedule;
mod safe_archive;
mod screenshot;
mod system;
mod traffic;
mod trusted_service_credentials;
mod trusted_service_database;
mod trusted_service_gateway;
mod trusted_service_oci;
mod trusted_service_runtime;
mod version;

use crate::api::{
    configured_repository_backend, router, ClientRepository, RepositoryBackend, RuntimeState,
};
use crate::db::Database;
use std::io::Read;
use std::sync::Arc;

const MAX_HOSTNAME_BYTES: usize = 4096;

fn parse_listen_addr(value: &str) -> Result<std::net::SocketAddr, String> {
    value
        .parse()
        .map_err(|error| format!("invalid SIGNAGE_LISTEN_ADDR: {error}"))
}

fn ensure_data_dir(path: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|error| format!("failed to create data directory: {error}"))
}

fn open_sqlite_database(path: &str) -> Result<Database, String> {
    Database::open(path).map_err(|error| format!("failed to open database: {error}"))
}

fn validate_tls_env_pair(cert: Option<&str>, key: Option<&str>) -> Result<(), String> {
    let cert_set = cert.is_some();
    let key_set = key.is_some();
    if cert_set != key_set {
        return Err("SIGNAGE_TLS_CERT and SIGNAGE_TLS_KEY must be set together".to_string());
    }
    Ok(())
}

fn read_bounded_hostname() -> Result<String, std::io::Error> {
    let file = std::fs::File::open("/etc/hostname")?;
    let mut bytes = Vec::new();
    file.take((MAX_HOSTNAME_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_HOSTNAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "hostname exceeds the configured limit",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

#[cfg(test)]
pub(crate) static PROCESS_ENV_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
    std::sync::OnceLock::new();

#[tokio::main]
async fn main() {
    // rustls 0.23+ requires a process-level crypto provider. We use ring.
    // Both reqwest (rustls-tls) and our direct axum-server dep share this
    // installation, so we set it once at startup.
    let _ = rustls::crypto::ring::default_provider().install_default();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Lightweight CLI probe used by the Version Manager update importer to
    // validate an uploaded replacement binary before swapping it into place.
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!(
            "{} {} commit={} build_time={}",
            crate::version::PKG_NAME,
            crate::version::VERSION,
            crate::version::GIT_COMMIT,
            crate::version::BUILD_TIME
        );
        return;
    }

    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--build-update-bundle") {
        let destination = args.get(index + 1).cloned().unwrap_or_else(|| {
            eprintln!("--build-update-bundle requires an output path");
            std::process::exit(2);
        });
        match crate::version::export_update_bundle(std::path::Path::new(&destination)) {
            Ok(manifest) => {
                println!("{}", serde_json::to_string_pretty(&manifest).unwrap_or_default());
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-schema") {
        match crate::postgres::apply_initial_schema().await {
            Ok(()) => println!("PostgreSQL schema applied"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-auth-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.auth_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL authentication repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-client-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.client_registration_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL client registration repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-assignment-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.client_assignment_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL client assignment repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-virtual-client-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.virtual_client_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL virtual-client repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-session-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.session_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL session repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-stale-client-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.stale_client_count_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL stale-client repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-maintenance-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.maintenance_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL maintenance repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-controls-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.client_controls_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL client controls repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-command-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.client_command_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL client command repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-scene-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.scene_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL scene repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-display-list-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.client_display_listing_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL client display listing repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-history-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.client_history_lifecycle_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL client history repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-admin-users-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.client_admin_users_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL client admin-user repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-display-control-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.display_control_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL display control repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-registered-clients-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.registered_clients_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL registered-clients repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-news-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.news_feed_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL news-feed repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-layout-folder-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.layout_folder_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL layout-folder repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-layout-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.layout_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL layout repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-layout-set-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.layout_set_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL layout-set repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-widget-preset-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.widget_preset_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL widget-preset repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-overrides-templates-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.display_override_template_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL display-override/template repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-schedule-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.schedule_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL schedule repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-submission-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.interactive_submission_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL interactive-submission repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-refresh-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.refresh_epoch_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL refresh-epoch repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-table-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.table_admin_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL table-administration repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-users-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.user_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL users repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-display-state-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.display_assignment_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL display-state repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--postgres-backup-smoke") {
        match crate::postgres::PostgresStore::connect().await {
            Ok(store) => match store.backup_round_trip_probe().await {
                Ok(()) => println!("PostgreSQL backup repository probe passed"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            },
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    let data_dir = std::env::var("SIGNAGE_DATA_DIR")
        .unwrap_or_else(|_| "/home/board/data".to_string());
    ensure_data_dir(std::path::Path::new(&data_dir)).unwrap_or_else(|error| {
        eprintln!("FATAL: failed to create data directory: {error}");
        std::process::exit(1);
    });

    // Purge trash files older than 14 days on startup
    {
        const MAX_TRASH_PURGE_ENTRIES: usize = 10_000;
        use std::time::{SystemTime, UNIX_EPOCH};
        let trash_dir = std::path::Path::new("trash");
        if trash_dir.exists() {
            let cutoff = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .saturating_sub(14 * 24 * 3600);
            for entry in std::fs::read_dir(trash_dir)
                .into_iter()
                .flatten()
                .flatten()
                .take(MAX_TRASH_PURGE_ENTRIES)
            {
                let path = entry.path();
                if path.is_file() {
                    if let Ok(meta) = std::fs::metadata(&path) {
                        if let Ok(modified) = meta.modified() {
                            let modified_secs = modified
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            if modified_secs < cutoff {
                                let _ = std::fs::remove_file(path);
                            }
                        }
                    }
                }
            }
        }
    }

    let repository_backend = configured_repository_backend().unwrap_or_else(|error| {
        eprintln!("FATAL: {error}");
        std::process::exit(1);
    });
    let runtime_state = match repository_backend {
        RepositoryBackend::Postgres => RuntimeState::from_env(None).await,
        RepositoryBackend::Sqlite => {
            let db_path = format!("{}/signage.db", data_dir);
            let database = open_sqlite_database(&db_path).unwrap_or_else(|error| {
                eprintln!("FATAL: failed to open database: {error}");
                std::process::exit(1);
            });
            RuntimeState::from_env(Some(Arc::new(database))).await
        }
    }
    .unwrap_or_else(|error| {
        eprintln!("FATAL: failed to initialize repository backend: {error}");
        std::process::exit(1);
    });
    crate::screenshot::init(256 * 1024 * 1024);

    match crate::addons::ensure_bundled_standard_addons() {
        Ok(addons) => log::info!(
            "Standard weather/traffic addons available: {}",
            addons.len()
        ),
        Err(error) => log::error!("Failed to install bundled standard addons: {error}"),
    }

    let scheduler_repository = runtime_state.client_repository();
    tokio::spawn(async move {
        crate::schedule::run_repository_scheduler(scheduler_repository).await;
    });

    // Background task: periodic maintenance. Every 15 minutes:
    // purge expired sessions and count stale clients. Both calls
    // are cheap no-ops when there's nothing to do, so the cost is
    // bounded. Cheap enough to skip clustering / leader election.
    let maintenance_repository = runtime_state.client_repository();
    let maintenance_auth = runtime_state.auth_repository();
    tokio::spawn(async move {
        crate::run_maintenance_loop(maintenance_repository, maintenance_auth).await;
    });

    // Background task: opt-in GitHub release updater. It is disabled by
    // default and only applies signed stable release bundles when enabled in
    // the System page.
    let update_repository = runtime_state.client_repository();
    tokio::spawn(async move {
        crate::github_update::run_auto_update_loop(update_repository).await;
    });

    // Background task: pre-cache Google Font previews. One weight
    // (400) of every catalog family is downloaded to
    // ./uploads/google-fonts-cache/ so the font picker can show
    // each family's real face in its preview. The loop is rate-
    // limited (~10 req/s) and self-throttles (only re-downloads
    // uncached families) so the cost on a steady-state system is
    // near zero.
    tokio::spawn(async move {
        crate::fonts::run_preview_cache_loop().await;
    });

    let state_for_3005 = runtime_state.clone();
    let app_router = router(runtime_state);
    let addr_str = std::env::var("SIGNAGE_LISTEN_ADDR").unwrap_or_else(|_| "[::]:3000".to_string());
    let addr = parse_listen_addr(&addr_str).unwrap_or_else(|error| {
        eprintln!("FATAL: {error}");
        std::process::exit(2);
    });
    let tls_disabled = std::env::var("SIGNAGE_TLS").as_deref() == Ok("0");
    // During a client migration, keep the legacy main HTTP listener while
    // exposing HTTPS on the secondary listener. Fresh installs leave this
    // unset, so both listeners follow SIGNAGE_TLS and remain HTTPS.
    let secondary_tls = std::env::var("SIGNAGE_SECONDARY_TLS")
        .map(|value| value != "0")
        .unwrap_or(!tls_disabled);

    // Resolve TLS material in priority order:
    //   1. SIGNAGE_TLS_CERT / SIGNAGE_TLS_KEY env vars (explicit override)
    //   2. <install_dir>/certs/signage-server.crt + key (next to the binary)
    //   3. repo-relative ./certs/signage-server.crt (dev runs out of target/)
    //   4. fall through to rcgen self-signed (NOT recommended; browsers
    //      will reject it). CAs published under
    //      /install/screentra-cast-signage-ca.crt will fail to verify it.
    //
    // SEARCH_PATHS lists candidates to scan for `signage-server.crt` /
    // `.key`. The first hit wins. install_dir is computed from the
    // binary's own path so this works whether you run from
    // `target/release/` or a system install under /usr/local/bin.
    let tls_cert_path = std::env::var("SIGNAGE_TLS_CERT").ok();
    let tls_key_path = std::env::var("SIGNAGE_TLS_KEY").ok();
    validate_tls_env_pair(tls_cert_path.as_deref(), tls_key_path.as_deref()).unwrap_or_else(|error| {
        eprintln!("FATAL: {error}");
        std::process::exit(1);
    });

    let (default_cert, default_key) = default_tls_paths();
    let (tls_cert_path, tls_key_path) = match (tls_cert_path, tls_key_path) {
        (Some(c), Some(k)) => (Some(c), Some(k)),
        (None, None) => match (default_cert, default_key) {
            (Some(c), Some(k)) => {
                log::info!(
                    "Using bundled TLS cert from {} + {} (set SIGNAGE_TLS_CERT/KEY to override)",
                    c,
                    k
                );
                (Some(c), Some(k))
            }
            _ => (None, None),
        },
        _ => {
            eprintln!("FATAL: SIGNAGE_TLS_CERT and SIGNAGE_TLS_KEY must be set together");
            std::process::exit(1);
        }
    };

    let discovery_name = std::env::var("SCREENTRA_CAST_SERVER_NAME")
        .or_else(|_| read_bounded_hostname())
        .unwrap_or_else(|_| "Screentra Cast".to_string());
    let discovery_ca = crate::discovery::load_published_ca(tls_cert_path.as_deref());
    let discovery_port = addr.port();
    let discovery_tls = !tls_disabled;
    tokio::spawn(async move {
        if let Err(error) = crate::discovery::run_server(
            discovery_port,
            discovery_tls,
            discovery_name,
            discovery_ca,
        )
        .await
        {
            log::warn!("LAN discovery listener stopped: {error}");
        }
    });

    // Use provided cert files if both are set, otherwise generate a
    // self-signed cert at startup. Connections are still encrypted; only
    // the browser trust flow changes (one-time "Not Secure" warning).
    let rustls_config = if let (Some(cert_path), Some(key_path)) = (&tls_cert_path, &tls_key_path) {
        log::info!(
            "Loading TLS cert from {} and key from {}",
            cert_path,
            key_path
        );
        axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path)
            .await
            .expect("failed to load TLS cert/key files")
    } else {
        log::warn!(
            "No TLS cert configured and no bundled cert found in any search path. \
             Generating an ephemeral self-signed cert at startup that browsers WILL \
             reject. To fix: place signage-server.crt + signage-server.key in \
             the binary's install dir, or set SIGNAGE_TLS_CERT/SIGNAGE_TLS_KEY."
        );
        let cert = rcgen::generate_simple_self_signed(vec![
            "11.1.1.53".to_string(),
            "debian-sign-software".to_string(),
            "localhost".to_string(),
            "signage-server".to_string(),
        ])
        .expect("failed to generate self-signed cert");
        let cert_pem = cert.cert.pem().into_bytes();
        let key_pem = cert.key_pair.serialize_pem().into_bytes();
        axum_server::tls_rustls::RustlsConfig::from_pem(cert_pem, key_pem)
            .await
            .expect("failed to build RustlsConfig from self-signed cert")
    };

    // Also serve on port 3005 (TLS terminator for direct browser access
    // and embedded webviews on player clients) - start in background
    let secondary_addr_str = std::env::var("SIGNAGE_SECONDARY_LISTEN_ADDR")
        .unwrap_or_else(|_| "[::]:3005".to_string());
    let addr_3005: std::net::SocketAddr = secondary_addr_str.parse().expect("invalid SIGNAGE_SECONDARY_LISTEN_ADDR");
    let rustls_config_3005 = rustls_config.clone();
    let router_3005 = router(state_for_3005);
    if !secondary_tls {
        let listener_3005 = tokio::net::TcpListener::bind(addr_3005).await.unwrap();
        tokio::spawn(async move {
            log::info!("Secondary HTTP server starting on :3005");
            axum::serve(listener_3005, router_3005).await.unwrap();
        });
    } else {
        tokio::spawn(async move {
            log::info!("Secondary HTTPS server starting on :3005");
            axum_server::bind_rustls(addr_3005, rustls_config_3005)
                .serve(router_3005.into_make_service())
                .await
                .unwrap();
        });
    }

    // Main server (port 3000) - this blocks
    if tls_disabled {
        log::info!(
            "SIGNAGE_TLS=0 -> starting PLAIN HTTP on {} (dual-stack IPv4+IPv6)",
            addr
        );
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        axum::serve(listener, app_router).await.unwrap();
    } else {
        let tls_mode = if tls_cert_path.is_some() && tls_key_path.is_some() {
            "configured certificate"
        } else {
            "ephemeral self-signed certificate"
        };
        log::info!(
            "Web server starting on https://{} (dual-stack IPv4+IPv6, {})",
            addr,
            tls_mode
        );
        axum_server::bind_rustls(addr, rustls_config)
            .serve(app_router.clone().into_make_service())
            .await
            .unwrap();
    }
}

/// Search well-known locations for `signage-server.crt` and
/// `signage-server.key`. Returns `(cert, key)` for the first hit where
/// both files exist. Search anchors (in order):
///   1. `<binary_dir>/certs/`        -- binary's own dir (system installs)
///   2. `<binary_dir>/../certs/`     -- one level up (cargo target/release/...)
///   3. `<binary_dir>/../../certs/`  -- two levels up (cargo target/release/...)
///   4. `<cwd>/certs/`               -- cwd relative (env-variable independent)
fn default_tls_paths() -> (Option<String>, Option<String>) {
    let cert_name = "signage-server.crt";
    let key_name = "signage-server.key";

    let mut anchors: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            anchors.push(dir.join("certs"));
            if let Some(parent) = dir.parent() {
                anchors.push(parent.join("certs"));
                if let Some(grand) = parent.parent() {
                    anchors.push(grand.join("certs"));
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        anchors.push(cwd.join("certs"));
    }
    // de-dup, preserve order
    let mut seen = std::collections::HashSet::new();
    anchors.retain(|p| seen.insert(p.clone()));

    for dir in anchors {
        let c = dir.join(cert_name);
        let k = dir.join(key_name);
        if c.exists() && k.exists() {
            return (
                Some(c.to_string_lossy().into_owned()),
                Some(k.to_string_lossy().into_owned()),
            );
        }
    }
    (None, None)
}

async fn refresh_repository_news_feeds(
    repository: &ClientRepository,
    max_age_secs: i64,
) -> Result<usize, String> {
    let feeds = repository.list_news_feeds().await?;
    let now = chrono::Utc::now();
    let mut updated = 0;
    for feed in feeds {
        let stale = chrono::DateTime::parse_from_rfc3339(&feed.last_fetched_at)
            .map(|timestamp| {
                now.signed_duration_since(timestamp.with_timezone(&chrono::Utc))
                    .num_seconds()
                    >= max_age_secs
            })
            .unwrap_or(true);
        if !stale {
            continue;
        }

        let feed_for_fetch = feed.clone();
        let result = tokio::task::spawn_blocking(move || {
            let client = reqwest::blocking::Client::builder()
                .user_agent("Screentra Cast/1.0 (news-ticker; +https://github.com/ripley-kiosk)")
                .timeout(std::time::Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| format!("news http client: {error}"))?;
            crate::news::fetch_one_update_with_client(&client, &feed_for_fetch)
        })
        .await
        .map_err(|error| format!("news refresh task failed: {error}"))?;

        match result {
            Ok(update) => {
                if !update.not_modified {
                    repository
                        .upsert_news_feed(
                            &feed.url,
                            &update.title,
                            &update.headlines_json,
                            update.new_etag.as_deref(),
                            update.new_last_modified.as_deref(),
                        )
                        .await?;
                    updated += 1;
                }
                repository
                    .mark_news_feed_fetched(&feed.url, &now.to_rfc3339())
                    .await?;
            }
            Err(error) => {
                log::warn!("Maintenance: news refresh {} failed: {error}", feed.url);
                repository.record_news_feed_error(&feed.url, &error).await?;
            }
        }
    }
    Ok(updated)
}

/// Background maintenance loop. Runs forever; never returns.
/// Currently:
///   * Purge expired sessions every 15 minutes.
///   * Count stale clients every 15 minutes (for the /api/system/health
///     endpoint to surface a "stale_clients" count).
/// Both operations are O(N) over small tables and complete in well
/// under a millisecond on the live DB. Cheap enough to run in every
/// replica without coordination.
pub(crate) async fn run_maintenance_loop(
    repository: crate::api::ClientRepository,
    auth_repository: crate::auth::AuthRepository,
) {
    // 15 minutes. Cheap to make longer; this is a sane balance
    // between "stale data on the API" and "DB chatter".
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(15 * 60));
    // Track the last time we refreshed news feeds. The fetch happens
    // every 5 minutes; we use a wall-clock check inside the loop so
    // the first refresh fires 5 minutes after startup (not 15).
    let mut last_news_refresh: std::time::Instant = std::time::Instant::now();

    loop {
        interval.tick().await;
        match auth_repository.purge_expired_sessions().await {
            Ok(n) if n > 0 => log::info!("Maintenance: purged {n} expired sessions"),
            Ok(_) => {}
            Err(e) => log::warn!("Maintenance: purge_expired_sessions failed: {e}"),
        }
        // Count stale clients (no heartbeat in the last 5 minutes).
        // We don't write anything here - the API just reads the count.
        match repository.count_stale_clients(5 * 60).await {
            Ok(n) => log::debug!("Maintenance: {n} clients have last_seen > 5m ago"),
            Err(e) => log::warn!("Maintenance: count_stale_clients failed: {e}"),
        }
        // Refresh news feeds every 5 minutes. The fetch is HTTP and can
        // be slow, so we run it on a blocking task so the rest of the
        // maintenance loop (and the rest of the server) keeps making
        // progress.
        if last_news_refresh.elapsed() >= std::time::Duration::from_secs(5 * 60) {
            last_news_refresh = std::time::Instant::now();
            match refresh_repository_news_feeds(&repository, 5 * 60).await {
                Ok(n) if n > 0 => log::info!("Maintenance: refreshed {n} news feeds"),
                Ok(_) => log::debug!("Maintenance: no news feeds needed refresh"),
                Err(e) => log::warn!("Maintenance: news refresh failed: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod startup_tests {
    use super::parse_listen_addr;

    #[test]
    fn invalid_listen_addr_is_rejected_without_panicking() {
        assert!(std::panic::catch_unwind(|| parse_listen_addr("not-a-socket")).is_ok());
    }

    #[test]
    fn invalid_data_dir_returns_an_error_without_panicking() {
        let result = std::panic::catch_unwind(|| {
            super::ensure_data_dir(std::path::Path::new("/dev/null/screentra-data"))
        });
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn invalid_sqlite_database_returns_an_error_without_panicking() {
        let result = std::panic::catch_unwind(|| super::open_sqlite_database("/dev/null"));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn mismatched_tls_environment_returns_an_error_without_panicking() {
        let result = std::panic::catch_unwind(|| {
            super::validate_tls_env_pair(Some("cert.pem"), None)
        });
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
}

