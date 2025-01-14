use crate::error::NetworkError;
use crate::prelude::{
    PeerConnectionType, PeerResponse, PeerSignal, SessionSecuritySettings, UdpMode,
};
use crate::proto::packet_processor::includes::VirtualConnectionType;
use crate::proto::packet_processor::peer::peer_cmd_packet::route_signal_response;
use crate::proto::packet_processor::PrimaryProcessorResult;
use crate::proto::peer::peer_layer::HyperNodePeerLayerInner;
use crate::proto::remote::Ticket;
use crate::proto::session::HdpSession;
use citadel_crypt::entropy_bank::SecurityLevel;
use citadel_crypt::stacked_ratchet::StackedRatchet;

#[cfg_attr(feature = "localhost-testing", tracing::instrument(target = "citadel", skip_all, ret, err, fields(is_server = session.is_server, implicated_cid = implicated_cid, target_cid = target_cid)))]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_response_phase_post_connect(
    peer_layer: &mut HyperNodePeerLayerInner,
    peer_conn_type: PeerConnectionType,
    ticket: Ticket,
    peer_response: PeerResponse,
    endpoint_security_level: SessionSecuritySettings,
    udp_enabled: UdpMode,
    implicated_cid: u64,
    target_cid: u64,
    timestamp: i64,
    session: &HdpSession,
    sess_hyper_ratchet: &StackedRatchet,
    security_level: SecurityLevel,
) -> Result<PrimaryProcessorResult, NetworkError> {
    // the signal is going to be routed from HyperLAN Client B to HyperLAN client A (response phase)
    route_signal_response(PeerSignal::PostConnect(peer_conn_type, Some(ticket), Some(peer_response), endpoint_security_level, udp_enabled), implicated_cid, target_cid, timestamp, ticket, peer_layer, session.clone(), sess_hyper_ratchet,
                          |this_sess, peer_sess, _original_tracked_posting| {
                              // when the route finishes, we need to update both sessions to allow high-level message-passing
                              // In other words, forge a virtual connection
                              // In order for routing of packets to be fast, we need to get the direct handles of the stream
                              // placed into the state_containers
                              if let Some(this_tcp_sender) = this_sess.to_primary_stream.clone() {
                                  if let Some(peer_tcp_sender) = peer_sess.to_primary_stream.clone() {
                                      let mut this_sess_state_container = inner_mut_state!(this_sess.state_container);
                                      let mut peer_sess_state_container = inner_mut_state!(peer_sess.state_container);

                                      // The UDP senders may not exist (e.g., TCP only mode)
                                      let this_udp_sender = this_sess_state_container.udp_primary_outbound_tx.clone();
                                      let peer_udp_sender = peer_sess_state_container.udp_primary_outbound_tx.clone();
                                      // rel to this local sess, the key = target_cid, then (implicated_cid, target_cid)
                                      let virtual_conn_relative_to_this = VirtualConnectionType::LocalGroupPeer(implicated_cid, target_cid);
                                      let virtual_conn_relative_to_peer = VirtualConnectionType::LocalGroupPeer(target_cid, implicated_cid);
                                      this_sess_state_container.insert_new_virtual_connection_as_server(target_cid, virtual_conn_relative_to_this, peer_udp_sender, peer_tcp_sender);
                                      peer_sess_state_container.insert_new_virtual_connection_as_server(implicated_cid, virtual_conn_relative_to_peer, this_udp_sender, this_tcp_sender);
                                      log::trace!(target: "citadel", "Virtual connection between {} <-> {} forged", implicated_cid, target_cid);
                                      // TODO: Ensure that, upon disconnect, the corresponding entry gets dropped in the connection table of not the dropped peer
                                  }
                              }
                          }, security_level).await
}
