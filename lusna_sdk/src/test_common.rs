#![allow(missing_docs)]

use crate::prelude::*;
use std::net::SocketAddr;
use crate::prefabs::server::empty_kernel::EmptyKernel;

#[allow(dead_code)]
pub fn setup_log() {
    std::env::set_var("RUST_LOG", "error,warn,info,trace");
    //std::env::set_var("RUST_LOG", "error");
    let _ = env_logger::try_init();
    log::trace!("TRACE enabled");
    log::info!("INFO enabled");
    log::warn!("WARN enabled");
    log::error!("ERROR enabled");
}

#[allow(dead_code)]
pub fn default_server_test_node(bind_addr: SocketAddr) -> NodeFuture {
    server_test_node(bind_addr, EmptyKernel::default())
}

#[allow(dead_code)]
pub fn server_test_node(bind_addr: SocketAddr, kernel: impl NetKernel) -> NodeFuture {
    NodeBuilder::default()
        .with_node_type(NodeType::Server(bind_addr))
        .build(kernel).unwrap()
}