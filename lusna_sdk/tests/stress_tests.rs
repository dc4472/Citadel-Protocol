#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering, AtomicUsize};
    use uuid::Uuid;
    use lusna_sdk::prefabs::client::single_connection::SingleClientServerConnectionKernel;
    use lusna_sdk::prelude::*;
    use hyxe_net::prelude::{NetworkError, SecureProtocolPacket, SecBuffer, SessionSecuritySettingsBuilder, UdpMode, SecrecyMode, KemAlgorithm, EncryptionAlgorithm};
    use rstest::rstest;
    use tokio::sync::Barrier;
    use std::sync::Arc;
    use serde::{Serialize, Deserialize};
    use rand::prelude::ThreadRng;
    use rand::Rng;
    use futures::{StreamExt, TryStreamExt};
    use hyxe_net::prelude::SyncIO;
    use std::net::SocketAddr;
    use lusna_sdk::prefabs::ClientServerRemote;
    use std::str::FromStr;
    use lusna_sdk::prefabs::server::client_connect_listener::ClientConnectListenerKernel;
    use std::future::Future;
    use std::time::Duration;
    use lusna_sdk::prefabs::client::PrefabFunctions;
    use lusna_sdk::prefabs::client::peer_connection::PeerConnectionKernel;
    use futures::prelude::stream::FuturesUnordered;
    use lusna_sdk::prefabs::client::broadcast::{GroupInitRequestType, BroadcastKernel};
    use std::collections::HashMap;
    use lusna_sdk::test_common::server_info;

    const MESSAGE_LEN: usize = 2000;

    #[derive(Serialize, Deserialize)]
    pub struct MessageTransfer {
        pub idx: u64,
        pub rand: Vec<u8>
    }

    impl MessageTransfer {
        pub fn create(idx: u64) -> SecureProtocolPacket {
            let rand = Self::create_rand(idx);
            rand.into()
        }

        pub fn create_secbuffer(idx: u64) -> SecBuffer {
            let rand = Self::create_rand(idx);
            rand.into()
        }

        fn create_rand(idx: u64)-> Vec<u8> {
            let mut rng = ThreadRng::default();
            let mut rand = vec![0u8; MESSAGE_LEN];
            rng.fill(rand.as_mut_slice());
            Self { idx, rand }.serialize_to_vector().unwrap()
        }

        pub fn receive(input: SecBuffer) -> Self {
            Self::deserialize_from_vector(input.as_ref()).unwrap()
        }
    }

    pub fn server_info_reactive<F, Fut>(on_channel_received: F) -> (NodeFuture<Box<dyn NetKernel>>, SocketAddr)
        where
            F: Fn(ConnectSuccess, ClientServerRemote) -> Fut + Send + Sync + 'static,
            Fut: Future<Output=Result<(), NetworkError>> + Send + Sync + 'static {
        let port = lusna_sdk::test_common::get_unused_tcp_port();
        let bind_addr = SocketAddr::from_str(&format!("127.0.0.1:{}", port)).unwrap();
        let server = lusna_sdk::test_common::server_test_node(bind_addr, Box::new(ClientConnectListenerKernel::new(on_channel_received)) as Box<dyn NetKernel>);
        (server, bind_addr)
    }

    async fn handle_send_receive_e2e(barrier: Arc<Barrier>, channel: PeerChannel, count: usize) -> Result<(), NetworkError> {
        let (tx, rx) = channel.split();

        for idx in 0..count {
            tx.send_message(MessageTransfer::create(idx as u64)).await?;
        }

        let mut cur_idx = 0usize;

        let mut rx = rx.take(count);
        while let Some(msg) = rx.next().await {
            log::info!("**~ Received message {} ~**", cur_idx);
            let msg = MessageTransfer::receive(msg);
            assert_eq!(msg.idx, cur_idx as u64);
            assert_eq!(msg.rand.len(), MESSAGE_LEN);
            cur_idx += 1;
        }

        assert_eq!(cur_idx as usize, count);
        let _ = barrier.wait().await;

        Ok(())
    }

    async fn handle_send_receive_group(barrier: Arc<Barrier>, channel: GroupChannel, count: usize, total_peers: usize) -> Result<(), NetworkError> {
        let _ = barrier.wait().await;
        let (tx, mut rx) = channel.split();

        for idx in 0..count {
            tx.send_message(MessageTransfer::create_secbuffer(idx as u64)).await?;
        }

        let mut counter = HashMap::new();

        while let Some(msg) = rx.next().await {
            match msg {
                GroupBroadcastPayload::Message { payload, sender } => {
                    let cur_idx = counter.entry(sender).or_insert(0usize);
                    log::info!("**~ Received message {} for {}~**", cur_idx, sender);
                    let msg = MessageTransfer::receive(payload);
                    // order is not guaranteed in group broadcasts. Do not use idx
                    //assert_eq!(msg.idx, *cur_idx as u64);
                    assert_eq!(msg.rand.len(), MESSAGE_LEN);
                    *cur_idx += 1;
                    if counter.values().all(|r| *r == count)  && counter.len() == total_peers - 1 {
                        break;
                    }
                }

                GroupBroadcastPayload::Event { payload } => {
                    if let GroupBroadcast::MessageResponse(..) = &payload {

                    } else {
                        panic!("Received invalid message type: {:?}", payload);
                    }
                }
            }
        }

        // we receive messages from n - 1 peers
        assert_eq!(counter.len(), total_peers - 1);
        for messages_received in counter.values() {
            assert_eq!(*messages_received, count);
        }

        let _ = barrier.wait().await;

        Ok(())
    }

    fn get_barrier() -> Arc<Barrier> {
        lusna_sdk::test_common::TEST_BARRIER.lock().clone().unwrap().inner
    }

    #[rstest]
    #[case(500, SecrecyMode::Perfect)]
    #[case(4000, SecrecyMode::BestEffort)]
    #[timeout(std::time::Duration::from_secs(90))]
    #[tokio::test(flavor="multi_thread")]
    async fn stress_test_c2s_messaging(#[case] message_count: usize,
                                       #[case] secrecy_mode: SecrecyMode,
                                       #[values(KemAlgorithm::Firesaber, KemAlgorithm::Kyber768_90s)]
                                       kem: KemAlgorithm,
                                       #[values(EncryptionAlgorithm::AES_GCM_256_SIV, EncryptionAlgorithm::Xchacha20Poly_1305)]
                                       enx: EncryptionAlgorithm) {

        lusna_sdk::test_common::setup_log();
        lusna_sdk::test_common::TestBarrier::setup(2);
        static CLIENT_SUCCESS: AtomicBool = AtomicBool::new(false);
        static SERVER_SUCCESS: AtomicBool = AtomicBool::new(false);
        CLIENT_SUCCESS.store(false, Ordering::SeqCst);
        SERVER_SUCCESS.store(false, Ordering::SeqCst);

        let (server, server_addr) = server_info_reactive(move |conn, remote| async move {
            log::info!("*** SERVER RECV CHANNEL ***");
            handle_send_receive_e2e(get_barrier(), conn.channel, message_count).await?;
            log::info!("***SERVER TEST SUCCESS***");
            SERVER_SUCCESS.store(true, Ordering::Relaxed);
            remote.shutdown_kernel().await
        });

        let uuid = Uuid::new_v4();
        let session_security = SessionSecuritySettingsBuilder::default()
            .with_secrecy_mode(secrecy_mode)
            .with_crypto_params(kem + enx)
            .build();

        let client_kernel = SingleClientServerConnectionKernel::new_passwordless(uuid, server_addr, UdpMode::Enabled,session_security,move |connection, remote| async move {
            log::info!("*** CLIENT RECV CHANNEL ***");
            handle_send_receive_e2e(get_barrier(), connection.channel, message_count).await?;
            log::info!("***CLIENT TEST SUCCESS***");
            CLIENT_SUCCESS.store(true, Ordering::Relaxed);
            remote.shutdown_kernel().await
        });

        let client = NodeBuilder::default().build(client_kernel).unwrap();

        let joined = futures::future::try_join(server, client);

        let _ = tokio::time::timeout(Duration::from_secs(120),joined).await.unwrap().unwrap();

        assert!(CLIENT_SUCCESS.load(Ordering::Relaxed));
        assert!(SERVER_SUCCESS.load(Ordering::Relaxed));
    }

    #[rstest]
    #[case(500, SecrecyMode::Perfect)]
    #[case(4000, SecrecyMode::BestEffort)]
    #[timeout(std::time::Duration::from_secs(90))]
    #[tokio::test(flavor="multi_thread")]
    async fn stress_test_p2p_messaging(#[case] message_count: usize,
                                       #[case] secrecy_mode: SecrecyMode,
                                       #[values(KemAlgorithm::Firesaber, KemAlgorithm::Kyber768_90s)]
                                       kem: KemAlgorithm,
                                       #[values(EncryptionAlgorithm::AES_GCM_256_SIV, EncryptionAlgorithm::Xchacha20Poly_1305)]
                                       enx: EncryptionAlgorithm) {

        lusna_sdk::test_common::setup_log();
        lusna_sdk::test_common::TestBarrier::setup(2);
        static CLIENT0_SUCCESS: AtomicBool = AtomicBool::new(false);
        static CLIENT1_SUCCESS: AtomicBool = AtomicBool::new(false);
        CLIENT0_SUCCESS.store(false, Ordering::SeqCst);
        CLIENT1_SUCCESS.store(false, Ordering::SeqCst);

        let (server, server_addr) = server_info();

        let uuid0 = Uuid::new_v4();
        let uuid1 = Uuid::new_v4();
        let session_security = SessionSecuritySettingsBuilder::default()
            .with_secrecy_mode(secrecy_mode)
            .with_crypto_params(kem + enx)
            .build();

        // TODO: SinglePeerConnectionKernel
        // to not hold up all conns
        let client_kernel0 = PeerConnectionKernel::new_passwordless(uuid0, server_addr, vec![uuid1.into()],UdpMode::Enabled,session_security,move |mut connection, remote| async move {
            handle_send_receive_e2e(get_barrier(), connection.recv().await.unwrap()?.channel, message_count).await?;
            log::info!("***CLIENT0 TEST SUCCESS***");
            CLIENT0_SUCCESS.store(true, Ordering::Relaxed);
            remote.shutdown_kernel().await
        });

        let client_kernel1 = PeerConnectionKernel::new_passwordless(uuid1, server_addr, vec![uuid0.into()], UdpMode::Enabled,session_security,move |mut connection, remote| async move {
            handle_send_receive_e2e(get_barrier(), connection.recv().await.unwrap()?.channel, message_count).await?;
            log::info!("***CLIENT1 TEST SUCCESS***");
            CLIENT1_SUCCESS.store(true, Ordering::Relaxed);
            remote.shutdown_kernel().await
        });

        let client0 = NodeBuilder::default().build(client_kernel0).unwrap();
        let client1 = NodeBuilder::default().build(client_kernel1).unwrap();
        let clients = futures::future::try_join(client0, client1);

        let task = async move {
            tokio::select! {
                server_res = server => Err(NetworkError::msg(format!("Server ended prematurely: {:?}", server_res.map(|_| ())))),
                client_res = clients => client_res.map(|_| ())
            }
        };

        let _ = tokio::time::timeout(Duration::from_secs(120),task).await.unwrap().unwrap();

        assert!(CLIENT0_SUCCESS.load(Ordering::Relaxed));
        assert!(CLIENT1_SUCCESS.load(Ordering::Relaxed));
    }

    #[rstest]
    #[case(500)]
    #[timeout(std::time::Duration::from_secs(90))]
    #[tokio::test(flavor="multi_thread")]
    async fn stress_test_group_broadcast(#[case] message_count: usize) {
        const PEER_COUNT: usize = 3;
        lusna_sdk::test_common::setup_log();
        lusna_sdk::test_common::TestBarrier::setup(PEER_COUNT);

        static CLIENT_SUCCESS: AtomicUsize = AtomicUsize::new(0);
        CLIENT_SUCCESS.store(0, Ordering::Relaxed);
        let (server, server_addr) = server_info();

        let client_kernels = FuturesUnordered::new();
        let total_peers = (0..PEER_COUNT).into_iter().map(|_| Uuid::new_v4()).collect::<Vec<Uuid>>();
        let group_id = Uuid::new_v4();

        for idx in 0..PEER_COUNT {
            let uuid = total_peers.get(idx).cloned().unwrap();

            let request = if idx == 0 {
                // invite list is empty since we will expect the users to post_register to us before attempting to join
                GroupInitRequestType::Create { local_user: UserIdentifier::from(uuid), invite_list: vec![], group_id, accept_registrations: true }
            } else {
                GroupInitRequestType::Join {
                    local_user: UserIdentifier::from(uuid),
                    owner: total_peers.get(0).cloned().unwrap().into(),
                    group_id,
                    do_peer_register: true
                }
            };

            let client_kernel = BroadcastKernel::new_passwordless_defaults(uuid, server_addr, request, move |channel,remote| async move {
                log::info!("***GROUP PEER {}={} CONNECT SUCCESS***", idx,uuid);
                // wait for every group member to connect to ensure all receive all messages
                handle_send_receive_group(get_barrier(), channel, message_count, PEER_COUNT).await?;
                let _ = CLIENT_SUCCESS.fetch_add(1, Ordering::Relaxed);
                remote.shutdown_kernel().await
            });

            let client = NodeBuilder::default().build(client_kernel).unwrap();

            client_kernels.push(async move {
                client.await.map(|_| ())
            });
        }

        let clients = Box::pin(async move {
            client_kernels.try_collect::<()>().await.map(|_| ())
        });

        let res = futures::future::try_select(server, clients).await;
        if let Err(err) = &res {
            match err {
                futures::future::Either::Left(left) => {
                    log::warn!("ERR-left: {:?}", &left.0);
                },

                futures::future::Either::Right(right) => {
                    log::warn!("ERR-right: {:?}", &right.0);
                }
            }
        }
        assert!(res.is_ok());
        assert_eq!(CLIENT_SUCCESS.load(Ordering::Relaxed), PEER_COUNT);
    }
}