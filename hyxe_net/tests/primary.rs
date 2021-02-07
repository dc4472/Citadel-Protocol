#![feature(try_trait, decl_macro, result_flattening)]

#[cfg(test)]
pub mod tests {
    use std::error::Error;
    use hyxe_net::hdp::hdp_server::{HdpServerRequest, HdpServerRemote, Ticket};
    use crate::tests::kernel::{TestKernel, TestContainer, ActionType};
    use hyxe_net::kernel::kernel_executor::KernelExecutor;
    use hyxe_nat::hypernode_type::HyperNodeType;
    use hyxe_user::account_manager::AccountManager;
    use hyxe_net::hdp::hdp_packet_processor::includes::{SocketAddr, Duration};
    use std::str::FromStr;
    use hyxe_net::proposed_credentials::ProposedCredentials;
    use secstr::SecVec;
    use hyxe_crypt::drill::SecurityLevel;
    use std::sync::Arc;
    use parking_lot::{RwLock, Mutex};
    use hyxe_net::functional::TriMap;
    use hyxe_net::hdp::peer::peer_layer::{PeerSignal, PeerConnectionType};
    use hyxe_net::hdp::peer::channel::PeerChannel;
    use hyxe_crypt::sec_bytes::SecBuffer;
    use futures::{StreamExt, SinkExt};
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering, AtomicBool};
    use crate::utils::AssertSendSafeFuture;
    use byteorder::ByteOrder;
    use hyxe_net::hdp::state_container::VirtualConnectionType;
    use hyxe_net::error::NetworkError;
    use hyxe_net::hdp::hdp_packet_processor::peer::group_broadcast::GroupBroadcast;

    const COUNT: usize = 500;
    const TIMEOUT_CNT_MS: usize = 10000 + (COUNT*50);

    fn setup_log() {
        std::env::set_var("RUST_LOG", "error,warn,info,trace");
        //std::env::set_var("RUST_LOG", "error");
        let _ = env_logger::try_init();
        log::trace!("TRACE enabled");
        log::info!("INFO enabled");
        log::warn!("WARN enabled");
        log::error!("ERROR enabled");
    }

    fn flatten_err<T, E: ToString>(err: Result<Result<T, NetworkError>, E>) -> Result<T, NetworkError> {
        err.map_err(|err| NetworkError::Generic(err.to_string())).flatten()
    }

    fn function(f: impl FnOnce(Arc<RwLock<TestContainer>>) -> Option<ActionType> + Send + 'static) -> ActionType {
        ActionType::Function(Box::new(f))
    }

    #[derive(Copy, Clone, Eq, PartialEq, Debug)]
    pub enum NodeType {
        Server,
        Client0,
        Client1,
        Client2
    }

    // The current problems with the algorithm is that after 6 packets are sent with no re-keying yet established, packets
    // 7,8,9,10 use version 0 even though the other adjacent endpoint no longer has version 0.
    //
    // If we allow only one packet sent per down-and-back, we greatly reduce the speed of the network
    #[tokio::test]
    async fn main() -> Result<(), Box<dyn Error>> {
        setup_log();
        let server_bind_addr = SocketAddr::from_str("127.0.0.1:33332").unwrap();
        let client0_bind_addr = SocketAddr::from_str("127.0.0.1:33333").unwrap();
        let client1_bind_addr = SocketAddr::from_str("127.0.0.1:33334").unwrap();
        let client2_bind_addr = SocketAddr::from_str("127.0.0.1:33335").unwrap();

        let security_level = SecurityLevel::LOW;
        let p2p_security_level = SecurityLevel::LOW;

        static CLIENT0_FULLNAME: &'static str = "Thomas P Braun (test)";
        static CLIENT0_USERNAME: &'static str = "nologik";
        static CLIENT0_PASSWORD: &'static str = "mrmoney10";

        static CLIENT1_FULLNAME: &'static str = "Thomas P Braun I (test)";
        static CLIENT1_USERNAME: &'static str = "nologik1";
        static CLIENT1_PASSWORD: &'static str = "mrmoney10";

        static CLIENT2_FULLNAME: &'static str = "Thomas P Braun II (test)";
        static CLIENT2_USERNAME: &'static str = "nologik2";
        static CLIENT2_PASSWORD: &'static str = "mrmoney10";

        let test_container = Arc::new(RwLock::new(TestContainer::default()));

        log::info!("Setting up executors ...");
        let server_executor = create_executor(server_bind_addr, Some(test_container.clone()),NodeType::Server, Vec::default()).await;
        log::info!("Done setting up server executor");
        let client0_executor = create_executor(client0_bind_addr, Some(test_container.clone()), NodeType::Client0, {
            vec![ActionType::Request(HdpServerRequest::RegisterToHypernode(server_bind_addr, ProposedCredentials::new_unchecked(CLIENT0_FULLNAME, CLIENT0_USERNAME, SecVec::new(Vec::from(CLIENT0_PASSWORD)), None), None, security_level)),
                 function(move |test_container| client0_action1(test_container, CLIENT0_USERNAME, CLIENT0_PASSWORD, security_level)),
                 function(move |test_container| client0_action2(test_container)),
                 function(move |test_container| client0_action3(test_container, p2p_security_level))
            ]
        }).await;

        let client1_executor = create_executor(client1_bind_addr, Some(test_container.clone()), NodeType::Client1, {
            vec![ActionType::Request(HdpServerRequest::RegisterToHypernode(server_bind_addr, ProposedCredentials::new_unchecked(CLIENT1_FULLNAME, CLIENT1_USERNAME, SecVec::new(Vec::from(CLIENT1_PASSWORD)), None), None, security_level)),
                 function(move |test_container| client1_action1(test_container, CLIENT1_USERNAME, CLIENT1_PASSWORD, security_level))
            ]
        }).await;

        let client2_executor = create_executor(client2_bind_addr, Some(test_container.clone()), NodeType::Client2, {
            vec![ActionType::Request(HdpServerRequest::RegisterToHypernode(server_bind_addr, ProposedCredentials::new_unchecked(CLIENT2_FULLNAME, CLIENT2_USERNAME, SecVec::new(Vec::from(CLIENT2_PASSWORD)), None), None, security_level)),
                 function(move |test_container| client2_action1(test_container, CLIENT2_USERNAME, CLIENT2_PASSWORD, security_level)),
                function(client2_action2),
                function(client2_action3)
            ]
        }).await;

        log::info!("Done setting up executors");

        let server_future = async move { server_executor.execute().await };
        let client0_future = tokio::task::spawn(tokio::time::timeout(Duration::from_millis(TIMEOUT_CNT_MS as u64), async move { client0_executor.execute().await }));
        let client1_future = tokio::task::spawn(tokio::time::timeout(Duration::from_millis(TIMEOUT_CNT_MS as u64), async move { client1_executor.execute().await }));
        let client2_future = tokio::task::spawn(tokio::time::timeout(Duration::from_millis(TIMEOUT_CNT_MS as u64), async move { client2_executor.execute().await }));

        let server = tokio::task::spawn(AssertSendSafeFuture::new_silent(server_future));
        tokio::time::delay_for(Duration::from_millis(100)).await;

        //futures::future::try_join_all(vec![client0_future, client1_future]).await.map(|res|)
        tokio::try_join!(client0_future, client1_future, client2_future)?.map(|res0, res1, res2| flatten_err(res0).and(flatten_err(res1)).and(flatten_err(res2)))?;

        log::info!("Ending test (client(s) done) ...");

        tokio::time::timeout(Duration::from_millis(100), server).await;

        Ok(())
    }

    async fn create_executor(bind_addr: SocketAddr, test_container: Option<Arc<RwLock<TestContainer>>>, node_type: NodeType, commands: Vec<ActionType>) -> KernelExecutor<TestKernel> {
        let account_manager = AccountManager::new(bind_addr, Some(format!("/Users/nologik/tmp/{}_{}", bind_addr.ip(), bind_addr.port()))).await.unwrap();
        account_manager.purge();
        let kernel = TestKernel::new(node_type, commands, test_container);
        KernelExecutor::new(HyperNodeType::BehindResidentialNAT, account_manager, kernel, bind_addr).await.unwrap()
    }

    pub mod kernel {
        use hyxe_net::kernel::kernel::NetKernel;
        use hyxe_net::hdp::hdp_server::{HdpServerRemote, HdpServerResult, HdpServerRequest, Ticket};
        use hyxe_net::error::NetworkError;
        use std::collections::HashSet;
        use parking_lot::{Mutex, RwLock};
        use hyxe_net::hdp::hdp_packet_processor::includes::Duration;
        use async_trait::async_trait;
        use std::sync::Arc;
        use hyxe_user::client_account::ClientNetworkAccount;
        use crate::tests::{NodeType, handle_peer_channel, COUNT, CLIENT_SERVER_MESSAGE_STRESS_TEST, start_client_server_stress_test};
        use hyxe_net::hdp::peer::peer_layer::{PeerSignal, PeerResponse};
        use hyxe_net::hdp::peer::channel::PeerChannel;
        use byteorder::ByteOrder;
        use hyxe_net::hdp::hdp_packet_processor::peer::group_broadcast::GroupBroadcast;

        #[derive(Default)]
        pub struct TestContainer {
            pub cnac_client0: Option<ClientNetworkAccount>,
            pub cnac_client1: Option<ClientNetworkAccount>,
            pub cnac_client2: Option<ClientNetworkAccount>,
            pub remote_client0: Option<HdpServerRemote>,
            pub remote_client1: Option<HdpServerRemote>,
            pub remote_client2: Option<HdpServerRemote>,
            pub queued_requests_client0: Option<Arc<Mutex<HashSet<Ticket>>>>,
            pub queued_requests_client1: Option<Arc<Mutex<HashSet<Ticket>>>>,
            pub queued_requests_client2: Option<Arc<Mutex<HashSet<Ticket>>>>,
            pub client_server_as_server_recv_count: usize,
            pub client_server_as_client_recv_count: usize,
            pub client_server_stress_test_done_as_client: bool,
            pub client_server_stress_test_done_as_server: bool
        }

        pub enum ActionType {
            Request(HdpServerRequest),
            Function(Box<dyn FnOnce(Arc<RwLock<TestContainer>>) -> Option<ActionType> + Send + 'static>)
        }

        pub struct TestKernel {
            node_type: NodeType,
            commands: Mutex<Vec<ActionType>>,
            remote: Option<HdpServerRemote>,
            // a ticket gets added once a request is submitted. Once a VALID response occurs, the entry is removed. If an invalid response is received, then the ticket lingers, then the timer throws an error
            queued_requests: Arc<Mutex<HashSet<Ticket>>>,
            item_container: Option<Arc<RwLock<TestContainer>>>,
            ctx_cid: Mutex<Option<u64>>
        }

        impl TestKernel {
            pub fn new(node_type: NodeType, commands: Vec<ActionType>, item_container: Option<Arc<RwLock<TestContainer>>>) -> Self {
                Self { node_type, commands: Mutex::new(commands), remote: None, queued_requests: Arc::new(Mutex::new(HashSet::new())), item_container, ctx_cid: Mutex::new(None) }
            }

            fn execute_action(&self, request: ActionType, remote: &HdpServerRemote) {
                match request {
                    ActionType::Request(request) => {
                        let ticket = remote.unbounded_send(request).unwrap();
                        assert!(self.queued_requests.lock().insert(ticket));
                    }

                    ActionType::Function(fx) => {
                        // execute the created action, or, run the next enqueued action
                        if let Some(request) = (fx)(self.item_container.clone().unwrap()) {
                            self.execute_action(request, remote);
                        } else {
                            self.execute_next_action();
                        }
                    }
                }
            }

            fn execute_next_action(&self) {
                if self.node_type != NodeType::Server {
                    let remote = self.remote.as_ref().unwrap();
                    let mut lock = self.commands.lock();
                    if lock.len() != 0 {
                        log::info!("[TEST] Executing next action for {:?} ...", self.node_type);
                        let item = lock.remove(0);
                        std::mem::drop(lock);
                        self.execute_action(item, remote);
                    }
                }
            }

            fn on_valid_ticket_received(&self, ticket: Ticket) {
                if self.node_type != NodeType::Server {
                    assert!(self.queued_requests.lock().remove(&ticket))
                }
            }

            #[allow(dead_code)]
            fn shutdown_in(&self, time: Option<Duration>) {
                let remote = self.remote.clone().unwrap();
                if let Some(time) = time {
                    tokio::task::spawn(async move {
                        tokio::time::delay_for(time).await;
                        remote.shutdown().unwrap();
                    });
                } else {
                    remote.shutdown().unwrap();
                }
            }
        }

        #[async_trait]
        impl NetKernel for TestKernel {
            async fn on_start(&mut self, server_remote: HdpServerRemote) -> Result<(), NetworkError> {
                log::info!("Running node {:?} onStart", self.node_type);
                if self.node_type != NodeType::Server {
                    self.execute_action(self.commands.lock().remove(0), &server_remote);
                    let container = self.item_container.as_ref().unwrap();
                    let mut write = container.write();
                    if self.node_type == NodeType::Client0 {
                        write.remote_client0 = Some(server_remote.clone());
                        write.queued_requests_client0 = Some(self.queued_requests.clone());
                    } else if self.node_type == NodeType::Client1 {
                        write.remote_client1 = Some(server_remote.clone());
                        write.queued_requests_client1 = Some(self.queued_requests.clone());
                    } else if self.node_type == NodeType::Client2 {
                        write.remote_client2 = Some(server_remote.clone());
                        write.queued_requests_client2 = Some(self.queued_requests.clone());
                    } else {
                        panic!("Unaccounted node type {:?}", self.node_type);
                    }
                }

                self.remote = Some(server_remote);
                Ok(())
            }

            async fn on_server_message_received(&self, message: HdpServerResult) -> Result<(), NetworkError> {
                log::info!("[{:?}] Message received: {:?}", self.node_type, &message);
                if self.node_type != NodeType::Server || message.is_message() {
                    match message {
                        HdpServerResult::ConnectFail(..) | HdpServerResult::RegisterFailure(..) => {
                            panic!("Register/Connect failed");
                        }

                        HdpServerResult::GroupEvent(implicated_cid, ticket, group) => {
                            match group {
                                GroupBroadcast::Invitation(key) => {
                                    log::info!("{:?} Received an invitation to group {}", self.node_type, key);
                                    // accept it
                                    if self.node_type == NodeType::Client0 || self.node_type == NodeType::Client1 {
                                        let accept = GroupBroadcast::AcceptMembership(key);
                                        let remote = self.remote.clone().unwrap();
                                        remote.send_with_custom_ticket(ticket, HdpServerRequest::GroupBroadcastCommand(implicated_cid, accept)).unwrap();
                                    } else {
                                        return Err(NetworkError::InternalError("Invalid group invitation recipient"));
                                    }
                                }

                                GroupBroadcast::CreateResponse(Some(key)) => {
                                    log::info!("Successfully created group {} for {:?}", key, self.node_type);
                                    self.on_valid_ticket_received(ticket);
                                    let remote = self.remote.clone().unwrap();

                                    tokio::task::spawn(async move {
                                        tokio::time::delay_for(Duration::from_millis(500)).await;
                                        remote.shutdown().unwrap();
                                    });
                                }

                                GroupBroadcast::AcceptMembershipResponse(success) => {
                                    log::info!("Successfully created group for {:?}? {}", self.node_type, success);
                                    let remote = self.remote.clone().unwrap();

                                    tokio::task::spawn(async move {
                                        tokio::time::delay_for(Duration::from_millis(500)).await;
                                        remote.shutdown().unwrap();
                                    });
                                }

                                _ => {}
                            }
                        }

                        HdpServerResult::MessageDelivery(_, _, msg) => {
                            let container = self.item_container.as_ref().unwrap();

                            let mut lock = container.write();

                            match self.node_type {
                                NodeType::Client0 => {
                                    // client0 already had its sender started. We only need to increment the inner count
                                    let val = byteorder::BigEndian::read_u64(msg.as_ref());
                                    assert_eq!(val, lock.client_server_as_client_recv_count as u64);
                                    lock.client_server_as_client_recv_count += 1;
                                    log::info!("[Client/Server Stress Test] RECV {} for {:?}", lock.client_server_as_client_recv_count, self.node_type);
                                    if lock.client_server_as_client_recv_count >= COUNT {
                                        lock.client_server_stress_test_done_as_client = true;
                                        let remote = self.remote.as_ref().unwrap().clone();
                                        assert!(self.queued_requests.lock().remove(&CLIENT_SERVER_MESSAGE_STRESS_TEST));
                                        std::mem::drop(lock);

                                        /*
                                        tokio::task::spawn(async move {
                                            tokio::time::delay_for(Duration::from_millis(1000)).await;
                                            remote.shutdown().unwrap();
                                        });*/
                                    }
                                }

                                NodeType::Server => {
                                    let val = byteorder::BigEndian::read_u64(msg.as_ref());
                                    assert_eq!(val, lock.client_server_as_server_recv_count as u64);
                                    lock.client_server_as_server_recv_count += 1;
                                    log::info!("[Client/Server Stress Test] RECV {} for {:?}", lock.client_server_as_server_recv_count, self.node_type);
                                    if lock.client_server_as_server_recv_count == 1 {
                                        lock.client_server_stress_test_done_as_server = true;
                                        // we must fire-up this side's subroutine for sending packets
                                        let client0_cid = lock.cnac_client0.as_ref().unwrap().get_cid();
                                        let queued_requests = self.queued_requests.clone();
                                        let remote = self.remote.clone().unwrap();
                                        let node_type = self.node_type;
                                        std::mem::drop(lock);
                                        tokio::task::spawn(async move {
                                            start_client_server_stress_test(queued_requests, remote, client0_cid, node_type).await;
                                        });
                                        return Ok(());
                                    }

                                    if lock.client_server_as_server_recv_count >= COUNT {
                                        log::info!("SERVER has finished receiving {} messages", COUNT);
                                        std::mem::drop(lock);
                                        assert!(self.queued_requests.lock().remove(&CLIENT_SERVER_MESSAGE_STRESS_TEST));

                                        /*
                                        tokio::task::spawn(async move {
                                            tokio::time::delay_for(Duration::from_millis(1000)).await;
                                            remote.shutdown().unwrap();
                                        });*/
                                    }

                                }

                                _ => {
                                    panic!("Invalid message delivery recipient {:?}", self.node_type);
                                }
                            }
                        }

                        HdpServerResult::InternalServerError(_, err) => {
                            panic!("Internal server error: {}", err);
                        }

                        HdpServerResult::RegisterOkay(ticket, cnac, _) => {
                            log::info!("SUCCESS registering ticket {} for {:?}", ticket, self.node_type);
                            // register the CID to be used in further checks
                            *self.ctx_cid.lock() = Some(cnac.get_cid());

                            if self.node_type == NodeType::Client0 {
                                self.item_container.as_ref().unwrap().write().cnac_client0 = Some(cnac);
                            } else if self.node_type == NodeType::Client1 {
                                self.item_container.as_ref().unwrap().write().cnac_client1 = Some(cnac);
                            } else if self.node_type == NodeType::Client2 {
                                self.item_container.as_ref().unwrap().write().cnac_client2 = Some(cnac);
                            } else {
                                panic!("Unaccounted node type: {:?}", self.node_type)
                            }

                            self.on_valid_ticket_received(ticket);
                        }

                        HdpServerResult::ConnectSuccess(ticket, ..) => {
                            log::info!("SUCCESS connecting ticket {} for {:?}", ticket, self.node_type);
                            self.on_valid_ticket_received(ticket);
                            if self.node_type != NodeType::Client1 && self.node_type != NodeType::Server {
                                // wait to ensure Client1 connects
                                tokio::time::delay_for(Duration::from_millis(100)).await;
                            }
                        }

                        HdpServerResult::PeerChannelCreated(ticket, channel) => {
                            self.on_valid_ticket_received(ticket);
                            handle_peer_channel(channel, self.remote.clone().unwrap(), self.item_container.clone().unwrap(), self.queued_requests.clone(), self.node_type);
                            //self.shutdown_in(Some(Duration::from_millis(1000)));
                        }

                        HdpServerResult::PeerEvent(signal, ticket) => {
                            match signal {
                                PeerSignal::PostRegister(vconn, _peer_username, _, resp_opt) => {
                                    if let Some(resp) = resp_opt {
                                        match resp {
                                            PeerResponse::Accept(_) => {
                                                log::info!("RECV PeerResponse::Accept for {:?}", self.node_type);
                                                self.on_valid_ticket_received(ticket);
                                                /*if self.node_type == NodeType::Client2 {
                                                    self.remote.as_ref().unwrap().shutdown().unwrap();
                                                }*/
                                            }

                                            _ => {
                                                log::error!("Invalid peer response for post-register")
                                            }
                                        }
                                    } else {
                                        log::info!("RECV PeerResponse::PostRegister for {:?} from {:?}", self.node_type, vconn);
                                        // the receiver of peer register requests is client 1 (c0 -> c1, c2 -> c1)
                                        assert_eq!(self.node_type, NodeType::Client1);
                                        let item_container = self.item_container.as_ref().unwrap();
                                        let read = item_container.read();
                                        let this_cnac = read.cnac_client1.as_ref().unwrap();

                                        let this_cid = this_cnac.get_cid();
                                        let this_username = this_cnac.get_username();
                                        let accept_post_register = HdpServerRequest::PeerCommand(this_cid, PeerSignal::PostRegister(vconn.reverse(), this_username, Some(ticket), Some(PeerResponse::Accept(None))));
                                        self.remote.as_ref().unwrap().send_with_custom_ticket(ticket, accept_post_register).unwrap();
                                        //self.shutdown_in(Some(Duration::from_millis(500)));
                                    }
                                }

                                PeerSignal::PostConnect(vconn, _, resp_opt, p2p_sec_lvl) => {
                                    if let None = resp_opt {
                                        // the receiver is client 1
                                        assert_eq!(self.node_type, NodeType::Client1);
                                        // receiver peer. ALlow the connection
                                        let item_container = self.item_container.as_ref().unwrap();
                                        let read = item_container.read();
                                        let this_cnac = read.cnac_client1.as_ref().unwrap();

                                        let this_cid = this_cnac.get_cid();
                                        let accept_post_connect = HdpServerRequest::PeerCommand(this_cid, PeerSignal::PostConnect(vconn.reverse(), Some(ticket), Some(PeerResponse::Accept(None)), p2p_sec_lvl));
                                        // we will expect a PeerChannel
                                        self.queued_requests.lock().insert(ticket);
                                        self.remote.as_ref().unwrap().send_with_custom_ticket(ticket, accept_post_connect).unwrap();
                                        //self.shutdown_in(Some(Duration::from_millis(1500)));
                                    }
                                }

                                PeerSignal::SignalReceived(_) => {}

                                PeerSignal::Disconnect(vconn, _) => {
                                    log::warn!("Peer vconn {} disconnected", vconn)
                                }

                                _ => {
                                    panic!("Unexpected signal: {:?}", signal);
                                }
                            }
                        }

                        _ => {
                            // prevent unaccounted signals from triggering next actions
                            return Ok(())
                        }
                    }

                    self.execute_next_action();
                }

                Ok(())
            }

            fn can_run(&self) -> bool {
                true
            }

            async fn on_stop(&self) -> Result<(), NetworkError> {
                if self.queued_requests.lock().len() != 0 || self.commands.lock().len() != 0 {
                    log::error!("ITEMS REMAIN (node type: {:?})", self.node_type);
                    Err(NetworkError::Generic(format!("Test error: items still in queue, or commands still pending for {:?}", self.node_type)))
                } else {
                    log::info!("NO ITEMS REMAIN (node type: {:?})", self.node_type);
                    Ok(())
                }
            }
        }
    }

    const CLIENT_SERVER_MESSAGE_STRESS_TEST: Ticket = Ticket(0xfffffffe);
    const P2P_MESSAGE_STRESS_TEST: Ticket = Ticket(0xffffffff);

    #[allow(unused_results)]
    pub fn handle_peer_channel(channel: PeerChannel, remote: HdpServerRemote, test_container: Arc<RwLock<TestContainer>>, requests: Arc<Mutex<HashSet<Ticket>>>, node_type: NodeType) {
        assert!(requests.lock().insert(P2P_MESSAGE_STRESS_TEST));

        tokio::task::spawn(async move {
            tokio::time::delay_for(Duration::from_millis(300)).await;
            let (sink, mut stream) = channel.split();
            let sender = async move {
                for x in 0..COUNT {
                    if x % 10 == 9 {
                        tokio::time::delay_for(Duration::from_millis(1)).await
                    }
                    //tokio::time::delay_for(Duration::from_millis(10)).await;
                    sink.send_unbounded(SecBuffer::from(&(x as u64).to_be_bytes() as &[u8])).unwrap();
                }

                log::info!("DONE sending {} messages for {:?}", COUNT, node_type);
                true
            };

            let receiver = async move {
                let messages_recv = Arc::new(AtomicUsize::new(0));

                while let Some(val) = stream.next().await {
                    match node_type {
                        NodeType::Client0 | NodeType::Client1 => {
                            let count_now = messages_recv.clone().fetch_add(1, Ordering::SeqCst) + 1;
                            log::info!("{:?} RECV MESSAGE {:?}. CUR COUNT: {}", val.as_ref(), node_type, count_now);
                            let value = byteorder::BigEndian::read_u64(val.as_ref()) as usize;
                            assert_eq!(count_now - 1, value);
                            if count_now >= COUNT {
                                break;
                            }
                        }

                        n => {
                            panic!("Unaccounted node type in p2p message handler: {:?}", n);
                        }
                    }
                }

                let count = messages_recv.load(Ordering::SeqCst);
                if count >= COUNT {
                    log::info!("DONE receiving {} messages for {:?}", COUNT, node_type);
                    true
                } else {
                    log::error!("Unable to receive all messages {:?}: {}/{}", node_type, count, COUNT);
                    false
                }
            };

            if tokio::join!(sender, receiver) == (true, true) {
                assert!(requests.lock().remove(&P2P_MESSAGE_STRESS_TEST));
                if node_type == NodeType::Client0 {
                    let implicated_cid = {
                        let read = test_container.read();
                        read.cnac_client0.as_ref().unwrap().get_cid()
                    };

                    // begin the sender
                    tokio::time::delay_for(Duration::from_millis(200)).await;
                    start_client_server_stress_test(requests.clone(), remote.clone(), implicated_cid, node_type).await;
                }
            } else {
                log::error!("One or more tx/rx failed for {:?}", node_type);
            }

            if node_type == NodeType::Client1 {
                tokio::time::delay_for(Duration::from_millis(1000)).await;
                remote.shutdown().unwrap();
            }
        });
    }

    const CLIENT_SERVER_STRESS_TEST_SEC: SecurityLevel = SecurityLevel::LOW;
    async fn start_client_server_stress_test(requests: Arc<Mutex<HashSet<Ticket>>>, mut remote: HdpServerRemote, implicated_cid: u64, node_type: NodeType) {
        assert!(requests.lock().insert(CLIENT_SERVER_MESSAGE_STRESS_TEST));
        log::info!("[Server/Client Stress Test] Starting send of {} messages on {:?} [target: {}]", COUNT, node_type, implicated_cid);
        let vals = &mut [0u8; 8];
        for x in 0..COUNT {
            byteorder::BigEndian::write_u64(vals as &mut [u8], x as u64);
            let next_ticket = remote.get_next_ticket();
            remote.send((next_ticket, HdpServerRequest::SendMessage(SecBuffer::from(vals.as_ref() as &[u8]), implicated_cid, VirtualConnectionType::HyperLANPeerToHyperLANServer(implicated_cid), CLIENT_SERVER_STRESS_TEST_SEC))).await.unwrap();
        }

        log::info!("Done sending {} messages as {:?}", COUNT, node_type)
    }

    #[allow(dead_code)]
    fn default_error(msg: &'static str) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::Other, msg)
    }

    fn client0_action1(item_container: Arc<RwLock<TestContainer>>, username: &str, password: &str, security_level: SecurityLevel) -> Option<ActionType> {
        let read = item_container.read();
        let cnac = read.cnac_client0.as_ref().unwrap();
        let read = cnac.read();
        let full_name = ""; // irrelevant for testing
        let ip = read.adjacent_nac.as_ref().unwrap().get_addr(true).unwrap();
        let cid = read.cid;
        let nonce = read.password_hash.as_slice();
        let proposed_credentials = ProposedCredentials::new_unchecked(full_name, username, SecVec::from(Vec::from(password)), Some(nonce));

        Some(ActionType::Request(HdpServerRequest::ConnectToHypernode(ip, cid, proposed_credentials, security_level, None, None, Some(true), None)))
    }

    fn client1_action1(item_container: Arc<RwLock<TestContainer>>, username: &str, password: &str, security_level: SecurityLevel) -> Option<ActionType> {
        let read = item_container.read();
        let cnac = read.cnac_client1.as_ref().unwrap();
        let read = cnac.read();
        let full_name = ""; // irrelevant for testing
        let ip = read.adjacent_nac.as_ref().unwrap().get_addr(true).unwrap();
        let cid = read.cid;
        let nonce = read.password_hash.as_slice();
        let proposed_credentials = ProposedCredentials::new_unchecked(full_name, username, SecVec::from(Vec::from(password)), Some(nonce));

        Some(ActionType::Request(HdpServerRequest::ConnectToHypernode(ip, cid, proposed_credentials, security_level, None, None, Some(true), None)))
    }

    fn client2_action1(item_container: Arc<RwLock<TestContainer>>, username: &str, password: &str, security_level: SecurityLevel) -> Option<ActionType> {
        let read = item_container.read();
        let cnac = read.cnac_client2.as_ref().unwrap();
        let read = cnac.read();
        let full_name = ""; // irrelevant for testing
        let ip = read.adjacent_nac.as_ref().unwrap().get_addr(true).unwrap();
        let cid = read.cid;
        let nonce = read.password_hash.as_slice();
        let proposed_credentials = ProposedCredentials::new_unchecked(full_name, username, SecVec::from(Vec::from(password)), Some(nonce));

        Some(ActionType::Request(HdpServerRequest::ConnectToHypernode(ip, cid, proposed_credentials, security_level, None, None, Some(true), None)))
    }

    // client 0 will initiate the p2p *registration* to client1
    fn client2_action2(item_container: Arc<RwLock<TestContainer>>) -> Option<ActionType> {
        tokio::task::spawn(async move {
            log::info!("Executing Client2_action2");
            loop {
                {
                    let write = item_container.write();
                    if let Some(cnac) = write.cnac_client1.as_ref() {
                        let client2_cnac = write.cnac_client2.as_ref().unwrap();
                        let client2_id = client2_cnac.get_cid();
                        let target_cid = cnac.get_cid();
                        let client2_username = client2_cnac.get_username();
                        let requests = write.queued_requests_client2.as_ref().unwrap();
                        let post_register_request = HdpServerRequest::PeerCommand(client2_id, PeerSignal::PostRegister(PeerConnectionType::HyperLANPeerToHyperLANPeer(client2_id, target_cid), client2_username, None, None));
                        let ticket = write.remote_client2.as_ref().unwrap().unbounded_send(post_register_request).unwrap();
                        assert!(requests.lock().insert(ticket));
                        return;
                    }
                }

                tokio::time::delay_for(Duration::from_millis(100)).await;
            }
        });

        None
    }

    /// Client 2 will patiently wait to send a group invite
    fn client2_action3(item_container: Arc<RwLock<TestContainer>>) -> Option<ActionType> {
        tokio::task::spawn(async move {
            loop {
                {
                    let read = item_container.read();
                    if read.client_server_stress_test_done_as_server && read.client_server_stress_test_done_as_client {
                        log::info!("[GROUP Stress test] Starting group stress test");
                        let client0_cnac = read.cnac_client0.as_ref().unwrap();
                        let client1_cnac = read.cnac_client1.as_ref().unwrap();
                        let this_cid = read.cnac_client2.as_ref().unwrap().get_cid();

                        let request = HdpServerRequest::GroupBroadcastCommand(this_cid, GroupBroadcast::Create(vec![client0_cnac.get_cid(), client1_cnac.get_cid()]));
                        let remote = read.remote_client2.clone().unwrap();
                        let ticket = remote.unbounded_send(request).unwrap();
                        assert!(read.queued_requests_client2.as_ref().unwrap().lock().insert(ticket));
                        return;
                    }
                }

                tokio::time::delay_for(Duration::from_millis(500)).await;
            }
        });

        None
    }

    // client 0 will initiate the p2p *registration* to client1
    fn client0_action2(item_container: Arc<RwLock<TestContainer>>) -> Option<ActionType> {
        tokio::task::spawn(async move {
            loop {
                {
                    let write = item_container.write();
                    if let Some(cnac) = write.cnac_client1.as_ref() {
                        let client0_cnac = write.cnac_client0.as_ref().unwrap();
                        let client0_id = client0_cnac.get_cid();
                        let target_cid = cnac.get_cid();
                        let client0_username = client0_cnac.get_username();
                        let requests = write.queued_requests_client0.as_ref().unwrap();
                        let post_register_request = HdpServerRequest::PeerCommand(client0_id, PeerSignal::PostRegister(PeerConnectionType::HyperLANPeerToHyperLANPeer(client0_id, target_cid), client0_username, None, None));
                        let ticket = write.remote_client0.as_ref().unwrap().unbounded_send(post_register_request).unwrap();
                        assert!(requests.lock().insert(ticket));
                        return;
                    }
                }

                tokio::time::delay_for(Duration::from_millis(100)).await;
            }
        });

        None
    }

    // client 0 will initiate the p2p *connection* to client1
    fn client0_action3(item_container: Arc<RwLock<TestContainer>>, p2p_security_level: SecurityLevel) -> Option<ActionType> {
        tokio::task::spawn(async move {
            loop {
                {
                    let read = item_container.read();
                    if let Some(cnac) = read.cnac_client1.as_ref() {
                        let client0_cnac = read.cnac_client0.as_ref().unwrap();
                        let client0_id = client0_cnac.get_cid();
                        let target_cid = cnac.get_cid();
                        let requests = read.queued_requests_client0.as_ref().unwrap();
                        let post_connect_request = HdpServerRequest::PeerCommand(client0_id, PeerSignal::PostConnect(PeerConnectionType::HyperLANPeerToHyperLANPeer(client0_id, target_cid), None, None, p2p_security_level));
                        let ticket = read.remote_client0.as_ref().unwrap().unbounded_send(post_connect_request).unwrap();
                        assert!(requests.lock().insert(ticket));
                        return;
                    }
                }
                // patiently wait for cnac_client1 to exist
                tokio::time::delay_for(Duration::from_millis(100)).await;
            }
        });

        None
    }
}

pub mod utils {
    use futures::Future;
    use std::pin::Pin;
    use futures::task::{Context, Poll};
    /// For denoting to the compiler that running the future is thread-safe
    /// It is up to the caller to ensure the supplied future is not going to be called
    /// from multiple threads concurrently. IF there is a single instance of the task, then
    /// use this. If there will be multiple, use the safer version in misc::ThreadSafeFuture
    pub struct AssertSendSafeFuture<'a, Out: 'a>(Pin<Box<dyn Future<Output=Out> + 'a>>);

    unsafe impl<'a, Out: 'a> Send for AssertSendSafeFuture<'a, Out> {}

    impl<'a, Out: 'a> AssertSendSafeFuture<'a, Out> {
        /// Wraps a future, asserting it is safe to use in a multithreaded context at the possible cost of race conditions, locks, etc
        pub unsafe fn new(fx: impl Future<Output=Out> + 'a) -> Self {
            Self(Box::pin(fx))
        }
        pub fn new_silent(fx: impl Future<Output=Out> + 'a) -> Self {
            Self(Box::pin(fx))
        }
    }

    impl<'a, Out: 'a> Future for AssertSendSafeFuture<'a, Out> {
        type Output = Out;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.0.as_mut().poll(cx)
        }
    }
}