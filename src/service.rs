//! Windows service integration. No-op on non-Windows targets.

#[cfg(windows)]
pub mod windows {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::Duration;

    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    pub const SERVICE_NAME: &str = "mssql-bridge";
    pub const SERVICE_DISPLAY_NAME: &str = "MSSQL Bridge";
    pub const SERVICE_DESCRIPTION: &str =
        "HTTP-to-MSSQL proxy. Thin REST bridge to a local SQL Server instance.";

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_service() {
            tracing::error!("service failed: {e:#}");
        }
    }

    fn run_service() -> anyhow::Result<()> {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let mut shutdown_tx = Some(shutdown_tx);

        let event_handler = move |control_event| -> ServiceControlHandlerResult {
            match control_event {
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    if let Some(tx) = shutdown_tx.take() {
                        let _ = tx.send(());
                    }
                    ServiceControlHandlerResult::NoError
                }
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };

        let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;

        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        let result = rt.block_on(async move {
            let config_path = resolve_config_path();
            let cfg = crate::config::Config::load(&config_path)?;
            crate::server::init_logging(&cfg.log.level)?;
            let state = crate::server::AppState::from_config(cfg.clone());

            let shutdown = async move {
                let _ = shutdown_rx.await;
                tracing::info!("shutdown signal received from SCM");
            };

            crate::server::serve(state, cfg.server.bind, shutdown).await
        });

        let _ = status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: match &result {
                Ok(_) => ServiceExitCode::Win32(0),
                Err(_) => ServiceExitCode::ServiceSpecific(1),
            },
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        });

        result
    }

    fn resolve_config_path() -> PathBuf {
        if let Ok(p) = std::env::var("MSSQL_BRIDGE_CONFIG") {
            return PathBuf::from(p);
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                return dir.join("config.toml");
            }
        }
        PathBuf::from("config.toml")
    }

    pub fn dispatch() -> anyhow::Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
        Ok(())
    }

    pub fn install(exe_path: PathBuf, config_path: Option<PathBuf>) -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )?;

        let mut launch_args: Vec<OsString> = vec!["service-run".into()];
        if let Some(p) = &config_path {
            launch_args.push("--config".into());
            launch_args.push(p.as_os_str().to_owned());
        }

        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(SERVICE_DISPLAY_NAME),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe_path,
            launch_arguments: launch_args,
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };

        let svc = manager.create_service(&info, ServiceAccess::CHANGE_CONFIG)?;
        svc.set_description(SERVICE_DESCRIPTION)?;
        Ok(())
    }

    pub fn uninstall() -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )?;
        let svc = manager.open_service(
            SERVICE_NAME,
            ServiceAccess::DELETE | ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
        )?;

        let status = svc.query_status()?;
        if status.current_state != ServiceState::Stopped {
            svc.stop()?;
            for _ in 0..30 {
                let s = svc.query_status()?;
                if s.current_state == ServiceState::Stopped {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }

        svc.delete()?;
        Ok(())
    }
}
