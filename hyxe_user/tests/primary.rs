#[cfg(test)]
mod tests {

    use hyxe_user::account_manager::AccountManager;
    use hyxe_fs::hyxe_crypt::hyper_ratchet::HyperRatchet;
    use std::net::{IpAddr, SocketAddr};
    use std::str::FromStr;
    use hyxe_user::client_account::ClientNetworkAccount;
    use dirs2::home_dir;
    use hyxe_crypt::hyper_ratchet::constructor::{BobToAliceTransferType, HyperRatchetConstructor};
    use hyxe_crypt::fcm::fcm_ratchet::{FcmRatchet, FcmRatchetConstructor};
    use hyxe_user::backend::{BackendType, PersistenceHandler};
    use rand::random;
    use hyxe_crypt::prelude::{SecBuffer, ConstructorOpts};
    use tokio::net::TcpListener;
    use ez_pqcrypto::algorithm_dictionary::CryptoParameters;
    use tokio::sync::Mutex;
    use hyxe_user::auth::proposed_credentials::ProposedCredentials;
    use futures::Future;
    use rstest::{rstest, fixture};
    use hyxe_user::misc::AccountError;
    use std::sync::Arc;

    static TEST_MUTEX: Mutex<()> = Mutex::const_new(());

    #[derive(Clone)]
    struct TestContainer {
        server_acc_mgr: AccountManager,
        client_acc_mgr: AccountManager,
        #[allow(dead_code)]
        // hold the tcp listeners for the duration of the test to ensure no re-binding during parallel tests
        tcp_listeners: Arc<(TcpListener, TcpListener)>
    }

    impl TestContainer {
        pub async fn new(server_backend: BackendType, client_backend: BackendType) -> Self {
            let server_bind = TcpListener::bind((IpAddr::from_str("127.0.0.1").unwrap(), 0)).await.unwrap();
            let client_bind = TcpListener::bind((IpAddr::from_str("127.0.0.1").unwrap(), 0)).await.unwrap();
            let server_acc_mgr = acc_mgr(server_bind.local_addr().unwrap(), server_backend).await;
            let client_acc_mgr = acc_mgr(client_bind.local_addr().unwrap(), client_backend).await;

            Self {
                server_acc_mgr,
                client_acc_mgr,
                tcp_listeners: Arc::new((server_bind, client_bind))
            }
        }

        pub async fn create_cnac(&self, username: &str, password: &str, full_name: &str) -> (ClientNetworkAccount, ClientNetworkAccount) {
            let client_nac = self.client_acc_mgr.get_local_nac().clone();
            let cid = random::<u64>();
            let (client_hr, server_hr) = gen(cid, 0, None);
            let server_vers = self.server_acc_mgr.register_impersonal_hyperlan_client_network_account(cid, client_nac.clone(), ProposedCredentials::new_register(full_name, username, SecBuffer::from(password)).await.unwrap(), server_hr, None).await.unwrap();
            let client_vers = self.client_acc_mgr.register_personal_hyperlan_server(cid, client_hr, ProposedCredentials::new_register(full_name, username, SecBuffer::from(password)).await.unwrap(), client_nac, None).await.unwrap();

            (client_vers, server_vers)
        }

        async fn deinit(self) {
            self.server_acc_mgr.purge_home_directory().await.unwrap();
            self.client_acc_mgr.purge_home_directory().await.unwrap();
        }

        #[allow(dead_code)]
        async fn purge(&self) {
            self.server_acc_mgr.purge().await.unwrap();
            self.client_acc_mgr.purge().await.unwrap();
        }
    }

    #[allow(unused_must_use)]
    fn setup_log() {
        std::env::set_var("RUST_LOG", "trace");
        let _ = env_logger::try_init();
        log::trace!("TRACE enabled");
        log::info!("INFO enabled");
        log::warn!("WARN enabled");
        log::error!("ERROR enabled");
    }

    fn get_possible_backend(env: &str, ty: &str) -> Vec<BackendType> {
        let mut backends = vec![BackendType::Filesystem];
        #[cfg(feature = "enterprise")] {
            match std::env::var(&env) {
                Ok(addr) => {
                    log::info!("Testing SQL ADDR ({}): {}", ty, addr);
                    backends.push(BackendType::sql(addr))
                }

                _ => {
                    log::error!("Make sure {} is set in the environment", env);
                    std::process::exit(1)
                }
            }
        }

        backends
    }

    fn client_backends() -> Vec<BackendType> {
        get_possible_backend("TESTING_SQL_SERVER_ADDR_CLIENT", "Client")
    }

    fn server_backends() -> Vec<BackendType> {
        get_possible_backend("TESTING_SQL_SERVER_ADDR_SERVER", "Server")
    }

    async fn test_harness<T, F>(mut t: T) -> Result<(), AccountError>
        where T: FnMut(TestContainer, PersistenceHandler, PersistenceHandler) -> F,
        F: Future<Output=Result<(), AccountError>> {
        setup_log();
        let _lock = TEST_MUTEX.lock().await;

        let client_backends = client_backends();
        let server_backends = server_backends();

        for client_backend in &client_backends {
            for server_backend in &server_backends {
                log::info!("Trying combination: client={:?} w/ server={:?}", client_backend, server_backend);
                let container = TestContainer::new(server_backend.clone(), client_backend.clone()).await;
                let (pers_cl, pers_se) = (container.client_acc_mgr.get_persistence_handler().clone(), container.server_acc_mgr.get_persistence_handler().clone());
                let res = (t)(container.clone(), pers_cl, pers_se).await;
                container.deinit().await;
                res?;
            }
        }

        Ok(())
    }

    #[tokio::test]
    async fn setup_account_managers() -> Result<(), AccountError> {
        test_harness(|_, _, _| async move { Ok(()) }).await
    }

    #[tokio::test]
    async fn test_cnac_creation() -> Result<(), AccountError> {
        test_harness(|container, _, _| async move {
            let (client, server) = container.create_cnac("nologik", "password", "Thomas P Braun").await;
            let lock_server = server.write();
            let lock_client = client.write();

            assert_eq!(lock_server.is_local_personal, false);
            assert_eq!(lock_client.is_local_personal, true);
            assert_eq!(lock_client.auth_store.username(), "nologik");
            assert_eq!(lock_server.auth_store.username(), "nologik");
            assert_eq!(lock_client.auth_store.full_name(), "Thomas P Braun");
            assert_eq!(lock_server.auth_store.full_name(), "Thomas P Braun");
            Ok(())
        }).await
    }

    #[tokio::test]
    async fn test_byte_map() -> Result<(), AccountError> {
        test_harness(|container, pers_cl, pers_se| async move {
            let (client, server) = container.create_cnac("nologik", "password", "Thomas P Braun").await;
            let dummy = Vec::from("Hello, world!");
            assert!(pers_cl.store_byte_map_value(client.get_cid(), 0821, "thekey", dummy.clone()).await.unwrap().is_none());
            assert_eq!(pers_cl.get_byte_map_value(client.get_cid(), 0821, "thekey").await.unwrap().unwrap(), dummy.clone());
            assert_eq!(pers_cl.get_byte_map_values_by_needle(client.get_cid(), 0821, "the").await.unwrap().remove("thekey").unwrap(), dummy.clone());
            assert_eq!(pers_cl.remove_byte_map_value(client.get_cid(), 0821, "thekey").await.unwrap().unwrap(), dummy.clone());
            assert!(pers_cl.remove_byte_map_value(client.get_cid(), 0821, "thekey").await.unwrap().is_none());
            Ok(())
        }).await
    }

    /*
    #[tokio::test]
    async fn fcm() {

        let acc_mgr_0 = acc_mgr(Some("1.2.2.0"), backend()).await;
        let acc_mgr_1 = acc_mgr(Some("1.2.2.1"), backend()).await;
        let user0 = create_cnac(Some(acc_mgr_0.clone())).await;
        let user1 = create_cnac(Some(acc_mgr_1.clone())).await;
        acc_mgr_0.register_hyperlan_p2p_at_endpoints(user0.get_cid(), user1.get_cid(), user1.get_username()).await.unwrap();
        acc_mgr_1.register_hyperlan_p2p_at_endpoints(user1.get_cid(), user0.get_cid(), user0.get_username()).await.unwrap();

        // now, create an HR for both
        let (hr_alice, hr_bob) = gen_fcm(user0.get_cid(), 0, Some(user1.get_cid()));
        assert_eq!(hr_alice.get_cid(), user0.get_cid());
        assert_eq!(hr_bob.get_cid(), user1.get_cid());
        assert_eq!(hr_alice.version(), hr_bob.version());

        let api_key = "AAAAsdc2buM:APA91bFGIgSp9drZGpM6rsTVWD_4A28QZVjBG9ty0ijwXn0k-peMNiivzCuSzojR7ESN13txcD7pZMyYJC_LPdjRk56EdXnUfIYDgVVbTN8VmWiVd82uJv2kEgcoGL-Flh1HXWZlVSf8";
        let alice_cl_key = "abcdefgh";
        let bob_cl_key = "e2xixMK2SKe66ryMXa1HUR:APA91bFeFWyYjh_9tPRGlDleBfUwiOLsBDXhA8__LmyYYiI8a9P4U2JcBPHtWjEnTbLxeF0ImX0nNemVZDBGxw3OYMb_UD0i2FG4UsqiOqbrMvY6dlWVYxWOy4PSDSSQS0cAgyndyR14";
        let alice_keys = FcmKeys::new(api_key, alice_cl_key);
        let bob_keys = FcmKeys::new(api_key, bob_cl_key);

        user0.visit_mut(|mut inner| {
            let toolset = Toolset::new(hr_alice.get_cid(), hr_alice.clone());
            let endpoint_container = PeerSessionCrypto::new_fcm(toolset, true, bob_keys);
            inner.fcm_crypt_container.insert(user1.get_cid(), endpoint_container);
        });

        user1.visit_mut(|mut inner| {
            let toolset = Toolset::new(hr_bob.get_cid(), hr_bob.clone());
            let endpoint_container = PeerSessionCrypto::new_fcm(toolset, false, alice_keys);
            inner.fcm_crypt_container.insert(user0.get_cid(), endpoint_container);
        });

        let input = "s6ZxbnhOXhcAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAADAAAAUAcAn+RvpBzkXNKr3cklyUhh/7Y/a/FcfEbgCguo//43hgR1C5QH9wnQobwnBj21htBMQV/Et9BKijQzudTdZbLXd8/DeMdQ4xlb3/XJXN6u+mEA12RnjySShx7AIYV+ZfKAT6VdKpLOHguUigjAOFjOlaaLFjTsLx6KtwyYiQ4/svwIUaBTnH5mYrRAjQOn1e+E8oeHoEOznw4scn/mVDIwhhCJ9oimmdEKVz5GVqhq9g5WZYUntT0hOgj4+WYtvqqu8rS0EqGWf16ODNGFmsW55+3kLxMm4Daw5S5QNVXSMId77Eoni+rcjepZeJebEo8yhqO3C3tbeBhcFJjuoMBt3tb3efk6mDY41oiRqZaTZfHUOTedZ3sguyPEZMrVa62ngRUcHHFheqe/7kHdrNtPuFLbI3+bIudNEaUqG/gk+H+6c3+4+/7mFXIItmyF1/+qRN0sOiv2BqUeE+IcKyvnpBWjWNBgJRBvzV31V2ayT0D1M/0Y2b4maULZA/7XHlwyF4qofTQBjPI+6bLs/jZky0DgPkU/9XP+56N30d5wKfGDyyhezFO2lDP4YG1W3udwMB2Mo0yM9C+Nwjs5bB4AUB4+Dmt/qLy2GyBDAIHMb87ULazfy3ZtfgOwnBNebwu+XmGJiA92Q0nn5LVjqvMuW/GPvdQXRF8f58DYLF758EO4GRwDXOYwY2sC468dGWdiGaNOezAZbK0wXvzsesEi6vMNEYuXsH3yvZwI6zyb4XlXZ32x/EG3+36EvAVUJArzlkWOuReToJKcMSFM0LUQYaLhqMHkMhO+pxsePcKuL4DQSIQVg9QxjITUuqDTTk3Wnj2m2hj2ujeHtKi2NPbVN9Sauzk7Tlgiz9c20FMDjfyH9WFpWZUu1pwdKYKIrvq4HF8gSX7As1Q7tG0fs/ikAAcE2c9ShMseax4zJ70d23qkdZqpgStT19l7upN2snm9/OU9ujXGgUQx7XF1XiusrVpEn+LoGMVPR2SzZpzbZ7Zjrx8pALscaOMM9Ud8nWYjCAsOPwxRShz0QGTm3rSKsU/0idza46czc8uZeJv3DVraF7hnhh8yZ+t29rql/2gbecr7F1JBtD/KrbM0u0EH+F0XUaB0aXIfA9YGL/aUR7WftfV045H1r+vwlBDme2UoXIIvTfK/uFS6vi8TSP9jBeYdzOQzwEWNaiB6kYGMVUaS94OWzgn3gTN3zNbhZJvOXVgtsj1U67N4GPmFaiYuy1TyRw1IlZLosvxjOn4mRmqa4wf+Ku0iBdSfklCt7PT0AJyxLYjemT255mx4euzGu8Z3ap4w5uUvOWNDiZJHDwnY+9jCox0TRzyVCImbdID43XsyLVEMoFBwPcAyrxTKi2DhXSR0es6+URWZWE3wqhbty8Km11oxI/2GfwtiwfMOnkypDszMglpXsDgzy6oZqzdbHeCzyJwQC4y+2NOlnTgzII9I3dh6alAudz41YE+VbL8nfP1RndFXEa03biSr9MBrXQ3Hpv9RHUdDwreT11CYu1HvL01ysROdtqejA1E9ZAG2Ihbz9i4pjYemxLgUchRKEq5b36udHpniCrAUkhRziNos26WaZX7MaI0xnNZmYlQ3hXkwCDe1/9MGYiqhou4J/ekWtFverTyqDrq+I2QTV0mAEF14hRrW+PXSjWK24JGklWHnRXTWGda6zmp+17rc+NzcN7cxzJMosXtWRwTE8kvCfpvK5JatKo6cq131c3WbqpsbV8hOISFcNXi6MooJGViyR3cHQbbEA0CYoxrZVhFXfLQDsaEJXyCXyB7robsgQHV+imyQ7NP/iwNKYOitrgI1xdhIp9PHBEgo4lE0CEc++LGY3/tvWdC8Uph/MUo1jI8G6CFJthjt+S2IXmUnrgQ/wQ36kL4gEzjV";
        // test blocking process
        log::info!("{:?}", hyxe_user::fcm::fcm_packet_processor::process(input, acc_mgr_0.clone()).await);

        // now, start the simulation
        user0.fcm_send_message_to(user1.get_cid(), SecBuffer::from("Hello, bob! From alice"), 0,acc_mgr_0.fcm_client()).await.unwrap();

        let _ = acc_mgr_0.purge().await.unwrap();
        let _ = acc_mgr_1.purge().await.unwrap();
    }*/

    #[allow(dead_code)]
    fn gen_fcm(cid: u64, version: u32, endpoint_bob_cid: Option<u64>) -> (FcmRatchet, FcmRatchet) {
        let opts = ConstructorOpts::new_init(None as Option<CryptoParameters>);
        let mut alice = FcmRatchetConstructor::new_alice(cid, version, opts.clone()).unwrap();
        let bob = FcmRatchetConstructor::new_bob(opts,alice.stage0_alice()).unwrap();
        alice.stage1_alice(&bob.stage0_bob().unwrap()).unwrap();
        let bob = if let Some(cid) = endpoint_bob_cid { bob.finish_with_custom_cid(cid).unwrap() } else { bob.finish().unwrap() };
        (alice.finish().unwrap(), bob)
    }

    fn gen(cid: u64, version: u32, endpoint_bob_cid: Option<u64>) -> (HyperRatchet, HyperRatchet) {
        let opts = ConstructorOpts::new_vec_init(None as Option<CryptoParameters>, 1);
        let mut alice = HyperRatchetConstructor::new_alice(opts.clone(), cid, version, None).unwrap();
        let bob = HyperRatchetConstructor::new_bob(cid,version, opts,alice.stage0_alice()).unwrap();
        alice.stage1_alice(&BobToAliceTransferType::Default(bob.stage0_bob().unwrap())).unwrap();
        let bob = if let Some(cid) = endpoint_bob_cid { bob.finish_with_custom_cid(cid).unwrap() } else { bob.finish().unwrap() };
        (alice.finish().unwrap(), bob)
    }

    async fn acc_mgr(addr: SocketAddr, backend: BackendType) -> AccountManager {
        let home_dir = format!("{}/tmp/{}", home_dir().unwrap().to_str().unwrap(), addr.to_string().replace(":", "p"));
        log::info!("Home dir: {}", &home_dir);
        AccountManager::new(addr, Some(home_dir), backend, None, None, None).await.unwrap()
    }
}