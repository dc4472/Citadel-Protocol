use rustls::{ClientConfig, ServerConfig, RootCertStore, Certificate, PrivateKey};
use crate::quic::{insecure::SkipServerVerification, generate_self_signed_cert};
use std::sync::Arc;
use tokio_rustls::{TlsConnector, TlsAcceptor};

/// Useful for allowing migration from a TLS config to a QUIC config in the hyxe_net crate
#[derive(Clone)]
pub struct TLSQUICInterop {
    pub tls_acceptor: TlsAcceptor,
    pub quic_chain: Vec<Certificate>,
    pub quic_priv_key: PrivateKey
}

pub fn create_client_dangerous_config() -> TlsConnector {
    let mut default = crate::quic::insecure::rustls_client_config();
    default.enable_sni = true;
    default.dangerous().set_certificate_verifier(SkipServerVerification::new());
    TlsConnector::from(Arc::new(default))
}

pub fn create_rustls_client_config(allowed_certs: &[&[u8]]) -> Result<ClientConfig, anyhow::Error> {
    let mut default = if allowed_certs.is_empty() {
        let mut root_store = RootCertStore::empty();
        let natives = rustls_native_certs::load_native_certs()?;
        for cert in natives {
            root_store.add(&rustls::Certificate(cert.0))?;
        }

        ClientConfig::builder().with_safe_defaults().with_root_certificates(root_store).with_no_client_auth()
    } else {
        let mut certs = rustls::RootCertStore::empty();
        for cert in allowed_certs {
            certs.add(&rustls::Certificate(cert.to_vec()))?;
        }
        rustls::ClientConfig::builder().with_safe_defaults().with_root_certificates(certs).with_no_client_auth()
    };

    default.enable_sni = true;
    Ok(default)
}

pub fn create_client_config(allowed_certs: &[&[u8]]) -> Result<TlsConnector, anyhow::Error> {
    Ok(TlsConnector::from(Arc::new(create_rustls_client_config(allowed_certs)?)))
}

pub fn create_server_self_signed_config() -> Result<TLSQUICInterop, anyhow::Error> {
    let (cert_der, priv_key_der) = generate_self_signed_cert()?;
    let (quic_chain, quic_priv_key) = crate::misc::cert_and_priv_key_der_to_quic_keys(&cert_der, &priv_key_der)?;

    // the server won't verify clients. The clients verify the server
    let server_config = ServerConfig::builder().with_safe_defaults().with_no_client_auth().with_single_cert(vec![Certificate(cert_der)], PrivateKey(priv_key_der))?;

    let ret = TLSQUICInterop {
        tls_acceptor: TlsAcceptor::from(Arc::new(server_config)),
        quic_chain: vec![quic_chain],
        quic_priv_key
    };

    Ok(ret)
}

pub fn create_server_config(pkcs12_der: &[u8], password: &str) -> Result<TLSQUICInterop, anyhow::Error> {
    let (certs_stack, cert, priv_key) = crate::misc::pkcs12_to_components(pkcs12_der, password)?;
    let (quic_chain, quic_priv_key) = crate::misc::pkcs_12_components_to_quic_keys(certs_stack.as_ref(), &cert, &priv_key)?;

    let server_config = ServerConfig::builder().with_safe_defaults().with_no_client_auth().with_single_cert(quic_chain.clone(), quic_priv_key.clone())?;

    let ret = TLSQUICInterop {
        tls_acceptor: TlsAcceptor::from(Arc::new(server_config)),
        quic_chain,
        quic_priv_key
    };

    Ok(ret)
}

#[cfg(test)]
mod tests {
    use crate::tls::create_server_self_signed_config;

    #[test]
    fn main() {
        let _ = create_server_self_signed_config().unwrap();
    }
}