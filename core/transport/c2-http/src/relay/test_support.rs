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

pub(crate) const TEST_ABI_HASH: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub(crate) const TEST_SIGNATURE_HASH: &str =
    "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

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
        ..RelayConfig::default()
    };
    Arc::new(RelayState::new(
        Arc::new(config),
        Arc::new(NoopDisseminator),
    ))
}

pub(crate) async fn register_echo_route(server: &Arc<Server>, name: &str) {
    register_echo_route_with_contract(
        server,
        name,
        "test.echo",
        "Echo",
        "0.1.0",
        TEST_ABI_HASH,
        TEST_SIGNATURE_HASH,
    )
    .await;
}

pub(crate) async fn register_echo_route_with_contract(
    server: &Arc<Server>,
    name: &str,
    crm_ns: &str,
    crm_name: &str,
    crm_ver: &str,
    abi_hash: &str,
    signature_hash: &str,
) {
    server
        .register_route(CrmRoute {
            name: name.into(),
            crm_ns: crm_ns.into(),
            crm_name: crm_name.into(),
            crm_ver: crm_ver.into(),
            abi_hash: abi_hash.into(),
            signature_hash: signature_hash.into(),
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
    let route_contracts = routes
        .iter()
        .map(|route| {
            (
                *route,
                "test.echo",
                "Echo",
                "0.1.0",
                TEST_ABI_HASH,
                TEST_SIGNATURE_HASH,
            )
        })
        .collect::<Vec<_>>();
    start_live_server_with_identity_and_contracts(
        address,
        server_id,
        &format!("{server_id}-instance"),
        &route_contracts,
    )
    .await
}

pub(crate) async fn start_live_server_with_identity_and_contracts(
    address: &str,
    server_id: &str,
    server_instance_id: &str,
    routes: &[(&str, &str, &str, &str, &str, &str)],
) -> Arc<Server> {
    let server = Arc::new(
        Server::new_with_identity(
            address,
            ServerIpcConfig::default(),
            c2_server::ServerIdentity {
                server_id: server_id.to_string(),
                server_instance_id: server_instance_id.to_string(),
            },
        )
        .unwrap(),
    );
    for (route, crm_ns, crm_name, crm_ver, abi_hash, signature_hash) in routes {
        register_echo_route_with_contract(
            &server,
            route,
            crm_ns,
            crm_name,
            crm_ver,
            abi_hash,
            signature_hash,
        )
        .await;
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
