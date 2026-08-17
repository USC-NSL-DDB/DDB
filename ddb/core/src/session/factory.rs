use anyhow::{bail, Context, Result};
use std::{net::Ipv4Addr, sync::Arc};

use super::{SessionMode, SessionProcess, SessionRequest, SessionRequestBuilder, SessionStart};
use crate::{
    cmd_flow::event::DebuggerEventReducer,
    common::config::{Config, DebuggerBackendKind, StaticSessionConfig, StaticSessionStartMode},
    common::counter::SimpleCounter,
    connection::{ssh_client::SSHCred, ssh_client_channel::SSHProxyCred},
    dbg_ctrl::{build_transport, ProxyTunnel, TransportSpec},
    debugger::DebuggerBackend,
    discovery::ServiceInfo,
    plugin::FrameworkPlugin,
    state::ServiceIdentity,
};

pub(crate) fn validate_static_session_config(
    config: &Config,
    session: &StaticSessionConfig,
) -> Result<()> {
    match config.conf.debugger.backend {
        DebuggerBackendKind::Mock => Ok(()),
        DebuggerBackendKind::Gdb | DebuggerBackendKind::Lldb => match session.start_mode {
            StaticSessionStartMode::Attach if session.pid == 0 => {
                bail!("static attach sessions require a non-zero pid")
            }
            StaticSessionStartMode::Binary if session.binary_path.trim().is_empty() => {
                bail!("static binary sessions require binary_path to be set")
            }
            StaticSessionStartMode::Binary => {
                let metadata = std::fs::metadata(&session.binary_path).with_context(|| {
                    format!("failed to inspect static binary {}", session.binary_path)
                })?;
                if !metadata.is_file() {
                    bail!(
                        "static binary {} is not a regular file",
                        session.binary_path
                    );
                }
                Ok(())
            }
            StaticSessionStartMode::Attach => Ok(()),
        },
        DebuggerBackendKind::Unknown => bail!("Unsupported debugger backend configured."),
    }
}

/// How sessions reach services reported by the active discovery source.
///
/// Producers report transport-agnostic facts; this policy, chosen from
/// configuration when discovery starts, decides the transport per service.
#[derive(Clone)]
pub(crate) enum DiscoveredTransportPolicy {
    DirectSsh {
        port: u16,
        user: String,
    },
    ProxySsh {
        tunnel: ProxyTunnel,
        port: u16,
        user: String,
        password: Option<String>,
    },
}

impl DiscoveredTransportPolicy {
    pub(crate) fn resolve(&self, ip: Ipv4Addr) -> (TransportSpec, Option<ProxyTunnel>) {
        match self {
            Self::DirectSsh { port, user } => (
                TransportSpec::DirectSsh(SSHCred::new(&ip.to_string(), *port, user, None)),
                None,
            ),
            Self::ProxySsh {
                tunnel,
                port,
                user,
                password,
            } => (
                TransportSpec::ProxySsh(SSHProxyCred::new(
                    &ip.to_string(),
                    u32::from(*port),
                    user,
                    None,
                    password.clone(),
                )),
                Some(Arc::clone(tunnel)),
            ),
        }
    }
}

/// Normalizes every session source into one validated process construction path.
#[derive(Clone)]
pub(crate) struct SessionFactory {
    config: Arc<Config>,
    backend: Arc<dyn DebuggerBackend>,
    plugin: Arc<dyn FrameworkPlugin>,
    reducer: Arc<DebuggerEventReducer>,
    session_ids: Arc<SimpleCounter>,
}

impl SessionFactory {
    pub(crate) fn new(
        config: Arc<Config>,
        backend: Arc<dyn DebuggerBackend>,
        plugin: Arc<dyn FrameworkPlugin>,
        reducer: Arc<DebuggerEventReducer>,
    ) -> Self {
        Self {
            config,
            backend,
            plugin,
            reducer,
            session_ids: Arc::new(SimpleCounter::new()),
        }
    }
    pub(crate) fn create_discovered(
        &self,
        info: ServiceInfo,
        transport: TransportSpec,
        proxy_tunnel: Option<ProxyTunnel>,
    ) -> Result<SessionProcess> {
        let request = self.build_discovery_request(info, transport)?;
        self.materialize(request, proxy_tunnel)
    }

    pub(crate) fn create_static(&self, session: StaticSessionConfig) -> Result<SessionProcess> {
        let request = self.build_static_request(session)?;
        self.materialize(request, None)
    }

    fn materialize(
        &self,
        request: SessionRequest,
        proxy_tunnel: Option<ProxyTunnel>,
    ) -> Result<SessionProcess> {
        let transport = build_transport(&request.transport, proxy_tunnel)?;
        Ok(SessionProcess::new(
            self.session_ids.next(),
            request,
            transport,
            Arc::clone(&self.config),
            Arc::clone(&self.backend),
            Arc::clone(&self.plugin),
            Arc::clone(&self.reducer),
        ))
    }

    fn build_discovery_request(
        &self,
        info: ServiceInfo,
        transport: TransportSpec,
    ) -> Result<SessionRequest> {
        let caladan_ip = info.caladan_ip();
        let service_identity = ServiceIdentity::new(info.hash, info.alias);

        SessionRequestBuilder::from_config(self.config.as_ref())
            .tag(info.tag)
            .mode(SessionMode::Remote(SessionStart::Attach(info.pid)))
            .transport(transport)
            .service_identity(service_identity)
            .caladan_ip(caladan_ip)
            .build()
    }

    pub(crate) fn build_static_request(
        &self,
        session: StaticSessionConfig,
    ) -> Result<SessionRequest> {
        validate_static_session_config(self.config.as_ref(), &session)?;
        let service_identity = ServiceIdentity::new(session.hash.clone(), session.alias.clone());
        let on_exit = session.on_exit.clone();

        let mut builder = SessionRequestBuilder::from_config(self.config.as_ref())
            .tag(session.tag)
            .stop_at_entry(session.stop_at_entry)
            .service_identity(service_identity);
        if let Some(on_exit) = on_exit {
            builder = builder.on_exit(on_exit);
        }

        builder = match self.config.conf.debugger.backend {
            DebuggerBackendKind::Mock => builder
                .mode(SessionMode::Remote(SessionStart::Attach(session.pid)))
                .transport(TransportSpec::Mock {
                    config: session.mock,
                    pid: session.pid,
                }),
            DebuggerBackendKind::Gdb | DebuggerBackendKind::Lldb => {
                let mode = match session.start_mode {
                    StaticSessionStartMode::Attach => {
                        SessionMode::Local(SessionStart::Attach(session.pid))
                    }
                    StaticSessionStartMode::Binary => SessionMode::Local(SessionStart::Binary {
                        path: session.binary_path,
                        args: session.binary_args,
                    }),
                };
                builder.mode(mode).transport(TransportSpec::Local)
            }
            DebuggerBackendKind::Unknown => bail!("Unsupported debugger backend configured."),
        };

        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, net::Ipv4Addr};

    use super::*;
    use crate::{
        cmd_flow::breakpoint::BreakpointEventPublisher, notification::NotificationManager,
        state::RuntimeModel,
    };

    fn test_factory(config: Config) -> SessionFactory {
        let config = Arc::new(config);
        let reducer = DebuggerEventReducer::new(
            RuntimeModel::new(),
            BreakpointEventPublisher::new(
                Arc::new(NotificationManager::new()),
                crate::cmd_flow::event_publisher::EventPublisher::spawn().0,
                crate::cmd_flow::output_hub::OutputHub::new(Default::default()),
            ),
        );
        SessionFactory::new(
            Arc::clone(&config),
            crate::debugger::resolve_debugger_backend(config.as_ref()).unwrap(),
            crate::plugin::resolve_framework_plugin(config.as_ref()),
            reducer,
        )
    }

    #[tokio::test]
    async fn static_attach_requires_a_pid() {
        let config = Config::default();
        let factory = test_factory(config);

        let error = factory
            .build_static_request(StaticSessionConfig::default())
            .expect_err("GDB attach without a pid should fail");

        assert_eq!(
            error.to_string(),
            "static attach sessions require a non-zero pid"
        );
    }

    #[tokio::test]
    async fn static_binary_requires_a_path() {
        let config = Config::default();
        let factory = test_factory(config);
        let session = StaticSessionConfig {
            start_mode: StaticSessionStartMode::Binary,
            ..StaticSessionConfig::default()
        };

        let error = factory
            .build_static_request(session)
            .expect_err("GDB binary launch without a path should fail");

        assert_eq!(
            error.to_string(),
            "static binary sessions require binary_path to be set"
        );
    }

    #[tokio::test]
    async fn discovery_metadata_carries_proclet_owner() {
        let config = Config::default();
        let factory = test_factory(config);
        let info = ServiceInfo::new(
            Ipv4Addr::LOCALHOST,
            "api".to_string(),
            42,
            "group".to_string(),
            "api".to_string(),
            Some(HashMap::from([(
                "caladan_ip".to_string(),
                "17".to_string(),
            )])),
        );

        let request = factory
            .build_discovery_request(info, TransportSpec::Local)
            .expect("discovery request should be valid");

        assert_eq!(request.caladan_ip, Some(17));
        assert_eq!(request.tag.as_deref(), Some("api"));
        assert_eq!(
            request.service_identity,
            Some(ServiceIdentity::new("group", "api"))
        );
    }

    #[tokio::test]
    async fn static_session_lifecycle_overrides_the_global_default() {
        let mut config = Config::default();
        config.conf.on_exit = crate::common::config::OnExit::DETACH;
        let factory = test_factory(config);
        let binary = tempfile::NamedTempFile::new().expect("fixture file should be created");
        let session = StaticSessionConfig {
            start_mode: StaticSessionStartMode::Binary,
            binary_path: binary.path().to_string_lossy().into_owned(),
            on_exit: Some(crate::common::config::OnExit::KILL),
            ..StaticSessionConfig::default()
        };

        let request = factory
            .build_static_request(session)
            .expect("valid session should build");

        assert_eq!(request.on_exit, crate::common::config::OnExit::KILL);
    }
}
