use citadel_sdk::prelude::*;
use std::net::SocketAddr;
use std::str::FromStr;

#[tokio::main]
async fn main() {
    citadel_logging::setup_log();
    let addr = get_env("CITADEL_SERVER_ADDR");
    let stun0 = get_env("STUN_0_ADDR");
    let stun1 = get_env("STUN_1_ADDR");
    let stun2 = get_env("STUN_2_ADDR");
    let server =
        citadel_sdk::prefabs::server::client_connect_listener::ClientConnectListenerKernel::new(
            |mut conn, _c2s_remote| async move {
                let chan = conn.udp_channel_rx.take();
                tokio::task::spawn(citadel_sdk::test_common::udp_mode_assertions(
                    UdpMode::Enabled,
                    chan,
                ))
                .await
                .map_err(|err| NetworkError::Generic(err.to_string()))?;
                Ok(())
            },
        );

    let _ = NodeBuilder::default()
        .with_node_type(NodeType::Server(
            SocketAddr::from_str(addr.as_str()).unwrap(),
        ))
        .with_stun_servers([stun0, stun1, stun2])
        .build(server)
        .unwrap()
        .await
        .unwrap();
}

fn get_env(key: &'static str) -> String {
    if let Ok(env) = std::env::var(key) {
        env
    } else {
        panic!("Expected the env_var {key} set")
    }
}
