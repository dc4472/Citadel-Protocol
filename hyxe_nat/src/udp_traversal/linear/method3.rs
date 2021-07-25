use std::net::SocketAddr;

use async_trait::async_trait;
use tokio::net::UdpSocket;
use tokio::time::Duration;

use crate::error::FirewallError;
use crate::udp_traversal::hole_punched_udp_socket_addr::HolePunchedSocketAddr;
use crate::udp_traversal::linear::{LinearUdpHolePunchImpl, RelativeNodeType};
use crate::udp_traversal::linear::encrypted_config_container::EncryptedConfigContainer;
use serde::{Serialize, Deserialize};
use std::sync::atomic::{AtomicBool, Ordering};

/// Method three: "Both sides send packets with short TTL values followed by packets with long TTL
// values". Source: page 7 of https://thomaspbraun.com/pdfs/NAT_Traversal/NAT_Traversal.pdf
pub struct Method3 {
    this_node_type: RelativeNodeType,
    encrypted_config: EncryptedConfigContainer
}

#[derive(Serialize, Deserialize)]
enum NatPacket {
    Syn(u32),
    // contains the local bind addr of candidate for socket identification
    SynAck(SocketAddr),
}


impl Method3 {
    /// Make sure to complete the pre-process stage before calling this
    pub fn new(this_node_type: RelativeNodeType, encrypted_config: EncryptedConfigContainer) -> Self {
        Self { this_node_type, encrypted_config }
    }

    /// The initiator must pass a vector correlating to the target endpoints. Each provided socket will attempt to reach out to the target endpoint (1-1)
    ///
    /// Note! The endpoints should be the port-predicted addrs
    async fn execute_either(&self, socket: &UdpSocket, endpoints: &Vec<SocketAddr>) -> Result<HolePunchedSocketAddr, FirewallError> {
        let default_ttl = socket.ttl().ok();
        // We will begin sending packets right away, assuming the pre-process synchronization occurred
        // 400ms window
        let ref encryptor = self.encrypted_config;
        let ref is_done = AtomicBool::new(false);

        let receiver_task = async move {

            // we are only interested in the first receiver to receive a value
            if let Ok(res) = tokio::time::timeout(Duration::from_millis(2000), Self::recv_until(socket, &endpoints[0], encryptor, is_done)).await.map_err(|err| FirewallError::HolePunch(err.to_string()))? {
                Ok(res)
            } else {
                Err(FirewallError::HolePunch("No UDP penetration detected".to_string()))
            }
        };

        let sender_task = async move {
            tokio::time::sleep(Duration::from_millis(10)).await; // wait to allow time for the joined receiver task to execute
            Self::send_syn_barrage(2, socket, endpoints, encryptor, 20, is_done).await.map_err(|err| FirewallError::HolePunch(err.to_string()))?;
            Self::send_syn_barrage(120, socket, endpoints, encryptor, 20, is_done).await.map_err(|err| FirewallError::HolePunch(err.to_string()))?;

            Ok(()) as Result<(), FirewallError>
        };

        let (res0, _) = tokio::join!(receiver_task, sender_task);
        let hole_punched_addr = res0?;

        if let Some(default_ttl) = default_ttl {
            socket.set_ttl(default_ttl).map_err(|err| FirewallError::HolePunch(err.to_string()))?;
        }

        log::info!("Completed hole-punch...");

        Ok(hole_punched_addr)
    }

    async fn send_syn_barrage(ttl: u32, socket: &UdpSocket, endpoints: &Vec<SocketAddr>, encryptor: &EncryptedConfigContainer, millis_delta: u64, _is_done: &AtomicBool) -> Result<(), anyhow::Error> {
        //let ref syn_packet = encryptor.generate_packet(&bincode2::serialize(&NatPacket::Syn(ttl)).unwrap());
        let _ = socket.set_ttl(ttl);
        let mut sleep = tokio::time::interval(Duration::from_millis(millis_delta));

        // fan-out of packets from a singular source to multiple consumers
        for _ in 0..5 {
            //if !is_done.load(Ordering::Relaxed) {
                let _ = sleep.tick().await;
                for endpoint in endpoints {
                    log::info!("Sending TTL={} to {}", ttl, endpoint);
                    socket.send_to(&encryptor.generate_packet(&bincode2::serialize(&NatPacket::Syn(ttl)).unwrap()), endpoint).await?;
                }
            //} else {
              //  break;
            //}
        }

        Ok(())
    }

    // Handles the reception of packets, as well as sending/awaiting for a verification
    async fn recv_until(socket: &UdpSocket, endpoint: &SocketAddr, encryptor: &EncryptedConfigContainer, is_done: &AtomicBool) -> Result<HolePunchedSocketAddr, FirewallError> {
        let buf = &mut [0u8; 4096];
        log::info!("[Hole-punch] Listening on {:?}", socket.local_addr().unwrap());

        let mut recv_from_required = None;
        while let Ok((len, nat_addr)) = socket.recv_from(buf).await {
            log::info!("[Hole-punch] RECV packet from {:?}", &nat_addr);
            let packet = match encryptor.decrypt_packet(&buf[..len]) {
                Some(plaintext) => plaintext,
                _ => {
                    log::warn!("BAD Hole-punch packet: decryption failed!");
                    continue;
                }
            };

            let packet: NatPacket = bincode2::deserialize(&packet).map_err(|err| FirewallError::HolePunch(err.to_string()))?;
            match packet {
                NatPacket::Syn(ttl) => {
                    log::info!("RECV SYN");
                    //if recv_from_required.is_none() {
                        log::info!("Received TTL={} packet. Awaiting mutual recognition...", ttl);
                        recv_from_required = Some(nat_addr);
                        // we received a packet, but, need to verify
                        for _ in 0..3 {
                            socket.send_to(&encryptor.generate_packet(&bincode2::serialize(&NatPacket::SynAck(socket.local_addr().unwrap())).unwrap()), nat_addr).await?;
                        }
                    //}
                }

                // the reception of a SynAck proves the existence of a hole punched since there is bidirectional communication through the NAT
                NatPacket::SynAck(bind_addr) => {
                    log::info!("RECV SYN_ACK");
                    if let Some(required_addr_in_conv) = recv_from_required {
                        if required_addr_in_conv == nat_addr {
                            // this means there was a successful ping-pong. We can now assume this communications line is valid since the nat addrs match
                            let initial_socket = endpoint;
                            let hole_punched_addr = HolePunchedSocketAddr::new(*initial_socket, nat_addr, bind_addr);
                            log::info!("***UDP Hole-punch to {:?} success!***", &hole_punched_addr);
                            is_done.store(true, Ordering::Relaxed);
                            return Ok(hole_punched_addr);
                        } else {
                            log::warn!("Received SynAck, but the addrs did not match!");
                        }
                    } else {
                        log::warn!("Received SynAck, but have not yet received Syn")
                    }
                }
            }
        }

        Err(FirewallError::HolePunch("Socket recv error".to_string()))
    }
}

#[async_trait]
impl LinearUdpHolePunchImpl for Method3 {
    async fn execute(&self, socket: &UdpSocket, endpoints: &Vec<SocketAddr>) -> Result<HolePunchedSocketAddr, FirewallError> {
        match self.this_node_type {
            RelativeNodeType::Initiator => {
                self.execute_either(socket, endpoints).await
            }

            RelativeNodeType::Receiver => {
                self.execute_either(socket, endpoints).await
            }
        }
    }
}