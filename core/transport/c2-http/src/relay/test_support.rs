use std::collections::HashMap;
use std::sync::Arc;

use c2_config::RelayConfig;
use c2_ipc::IpcClient;
use c2_server::{
    ConcurrencyMode, CrmCallback, CrmError, CrmRoute, RequestData, ResponseMeta, Scheduler, Server,
    ServerIpcConfig,
};

use crate::relay::disseminator::Disseminator;
use crate::relay::peer::PeerEnvelope;
use crate::relay::state::RelayState;
use crate::relay::types::PeerSnapshot;

pub(crate) struct NoopDisseminator;

impl Disseminator for NoopDisseminator {
    fn broadcast(
        &self,
        _envelope: PeerEnvelope,
        _peers: &[PeerSnapshot],
    ) -> Option<tokio::task::JoinHandle<()>> {
        None
    }
}

struct Echo;

impl CrmCallback for Echo {
    fn invoke(
        &self,
        _route_name: &str,
        _method_idx: u16,
        _request: RequestData,
        _response_pool: Arc<parking_lot::RwLock<c2_mem::MemPool>>,
    ) -> Result<ResponseMeta, CrmError> {
        Ok(ResponseMeta::Inline(b"echo".to_vec()))
    }
}

pub(crate) fn test_state_for_client() -> Arc<RelayState> {
    let config = RelayConfig {
        relay_id: "test-relay".into(),
        skip_ipc_validation: false,
        ..RelayConfig::default()
    };
    Arc::new(RelayState::new(
        Arc::new(config),
        Arc::new(NoopDisseminator),
    ))
}

pub(crate) async fn register_echo_route(server: &Arc<Server>, name: &str) {
    server
        .register_route(CrmRoute {
            name: name.into(),
            scheduler: Arc::new(Scheduler::new(
                ConcurrencyMode::ReadParallel,
                HashMap::new(),
            )),
            callback: Arc::new(Echo),
            method_names: vec!["ping".into()],
        })
        .await
        .unwrap();
}

pub(crate) async fn start_live_server_with_routes(
    address: &str,
    server_id: &str,
    routes: &[&str],
) -> Arc<Server> {
    let server = Arc::new(
        Server::new_with_identity(
            address,
            ServerIpcConfig::default(),
            c2_server::ServerIdentity {
                server_id: server_id.to_string(),
                server_instance_id: format!("{server_id}-instance"),
            },
        )
        .unwrap(),
    );
    for route in routes {
        register_echo_route(&server, route).await;
    }
    let run_server = server.clone();
    tokio::spawn(async move {
        let _ = run_server.run().await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let mut client = IpcClient::new(address);
            if client.connect().await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    server
}

pub(crate) async fn start_live_server(address: &str, server_id: &str) -> Arc<Server> {
    start_live_server_with_routes(address, server_id, &["grid"]).await
}
