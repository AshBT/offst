use futures::task::Spawn;

use proto::consts::TICKS_TO_REKEY;

use crypto::identity::PublicKey;
use crypto::crypto_rand::CryptoRandom;

use identity::IdentityClient;

use timer::TimerClient;
use timer::utils::sleep_ticks;

use common::connector::{Connector, ConnPair, BoxFuture};
use relay::client::client_connector::ClientConnector;

use secure_channel::create_secure_channel;


async fn secure_connect<C,A,R,S>(mut client_connector: C,
                            timer_client: TimerClient,
                            address: A,
                            public_key: PublicKey,
                            identity_client: IdentityClient,
                            rng: R,
                            spawner: S) -> Option<ConnPair<Vec<u8>, Vec<u8>>>
where
    A: Clone,
    C: Connector<Address=(A, PublicKey), SendItem=Vec<u8>, RecvItem=Vec<u8>>,
    R: CryptoRandom + 'static,
    S: Spawn + Clone + Sync + Send,
{
    let conn_pair = await!(client_connector.connect((address, public_key.clone())))?;
    match await!(create_secure_channel(conn_pair.sender, conn_pair.receiver,
                          identity_client,
                          Some(public_key.clone()),
                          rng,
                          timer_client,
                          TICKS_TO_REKEY,
                          spawner)) {
        Ok((sender, receiver)) => Some(ConnPair {sender, receiver}),
        Err(e) => {
            error!("Error in create_secure_channel: {:?}", e);
            None
        },
    }
}

#[derive(Clone)]
pub struct ChannelerConnector<C,R,S> {
    connector: C,
    keepalive_ticks: usize,
    backoff_ticks: usize,
    timer_client: TimerClient,
    identity_client: IdentityClient,
    rng: R,
    spawner: S,
}

impl<C,R,S> ChannelerConnector<C,R,S> {
    pub fn new(connector: C,
               keepalive_ticks: usize,
               backoff_ticks: usize,
               timer_client: TimerClient,
               identity_client: IdentityClient,
               rng: R,
               spawner: S) -> ChannelerConnector<C,R,S> {

        ChannelerConnector {
            connector,
            keepalive_ticks,
            backoff_ticks,
            timer_client,
            identity_client,
            rng,
            spawner,
        }
    }
}

impl<A,C,R,S> Connector for ChannelerConnector<C,R,S> 
where
    A: Sync + Send + Clone + 'static,
    C: Connector<Address=A, SendItem=Vec<u8>, RecvItem=Vec<u8>> + Clone + Send + Sync + 'static,
    R: CryptoRandom + 'static,
    S: Spawn + Clone + Sync + Send,
{
    type Address = (A, PublicKey);
    type SendItem = Vec<u8>;
    type RecvItem = Vec<u8>;

    fn connect(&mut self, address: (A, PublicKey)) 
        -> BoxFuture<'_, Option<ConnPair<Vec<u8>, Vec<u8>>>> {

        let (relay_address, public_key) = address;

        let client_connector = ClientConnector::new(
            self.connector.clone(), self.spawner.clone(), self.timer_client.clone(), self.keepalive_ticks);

        Box::pinned(async move {
            loop {
                match await!(secure_connect(client_connector.clone(), self.timer_client.clone(), relay_address.clone(), 
                                               public_key.clone(), self.identity_client.clone(), self.rng.clone(), self.spawner.clone())) {
                    Some(conn_pair) => return Some(conn_pair),
                    None => await!(sleep_ticks(self.backoff_ticks, self.timer_client.clone())).unwrap(),
                }
            }
        })
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::ThreadPool;
    use futures::channel::mpsc;

    use crypto::test_utils::DummyRandom;
    use crypto::identity::{SoftwareEd25519Identity,
                            generate_pkcs8_key_pair, PUBLIC_KEY_LEN,
                            PublicKey};
    use identity::create_identity;
    use timer::create_timer_incoming;
    use crypto::crypto_rand::RngContainer;

    async fn task_channeler_connector_basic(spawner: impl Spawn + Clone) {
        /*
        // Create a mock time service:
        let (tick_sender, tick_receiver) = mpsc::channel::<()>(0);
        let timer_client = create_timer_incoming(tick_receiver, spawner.clone()).unwrap();

        let rng = RngContainer::new(DummyRandom::new(&[1u8]));
        let pkcs8 = generate_pkcs8_key_pair(&rng);
        let identity = SoftwareEd25519Identity::from_pkcs8(&pkcs8).unwrap();
        let (requests_sender, identity_server) = create_identity(identity);
        let identity_client = IdentityClient::new(requests_sender);

        let backoff_ticks = 2;
        let keepalive_ticks = 8;

        let channeler_connector = ChannelerConnector::new(
            connector,
            keepalive_ticks,
            backoff_ticks,
            timer_client,
            identity_client,
            rng,
            spawner);
            */

    }

    #[test]
    fn test_channeler_connector_basic() {
        let mut thread_pool = ThreadPool::new().unwrap();
        thread_pool.run(task_channeler_connector_basic(thread_pool.clone()));
    }
}
