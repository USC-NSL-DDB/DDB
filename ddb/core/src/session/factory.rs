use anyhow::{bail, Result};
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

    fn build_static_request(&self, session: StaticSessionConfig) -> Result<SessionRequest> {
        let service_identity = ServiceIdentity::new(session.hash.clone(), session.alias.clone());

        let mut builder = SessionRequestBuilder::from_config(self.config.as_ref())
            .tag(session.tag)
            .stop_at_entry(session.stop_at_entry)
            .service_identity(service_identity);

        builder = match self.config.conf.debugger.backend {
            DebuggerBackendKind::Mock => builder
                .mode(SessionMode::Remote(SessionStart::Attach(session.pid)))
                .transport(TransportSpec::Mock {
                    config: session.mock,
                    pid: session.pid,
                }),
            DebuggerBackendKind::Gdb => {
                let mode = match session.start_mode {
                    StaticSessionStartMode::Attach => {
                        if session.pid == 0 {
                            bail!("static attach sessions require a non-zero pid");
                        }
                        SessionMode::Local(SessionStart::Attach(session.pid))
                    }
                    StaticSessionStartMode::Binary => {
                        if session.binary_path.trim().is_empty() {
                            bail!("static binary sessions require binary_path to be set");
                        }
                        SessionMode::Local(SessionStart::Binary {
                            path: session.binary_path,
                            args: session.binary_args,
                        })
                    }
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
}
