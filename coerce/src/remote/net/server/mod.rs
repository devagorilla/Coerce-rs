use crate::actor::{IntoActor, LocalActorRef};
use crate::remote::net::server::session::store::{NewSession, RemoteSessionStore};
use crate::remote::net::server::session::RemoteSession;
use crate::remote::system::RemoteActorSystem;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod session;

pub struct RemoteServer {
    cancellation_token: CancellationToken,
}

#[derive(Debug)]
pub enum RemoteServerErr {
    Startup,
    StreamErr(tokio::io::Error),
}

pub type RemoteServerConfigRef = Arc<RemoteServerConfig>;

#[derive(Debug)]
pub struct RemoteServerConfig {
    /// The address to listen for Coerce cluster client connections
    pub listen_addr: String,

    /// The address advertised by this node via the handshake
    pub external_node_addr: String,

    /// When true, incoming node addresses will be overwritten with the IP address
    /// used by the inbound client, rather than the address provided by
    /// the node via the handshake.
    pub override_incoming_node_addr: bool,
}

impl RemoteServerConfig {
    pub fn new(
        listen_addr: String,
        external_node_addr: String,
        override_incoming_node_addr: bool,
    ) -> Self {
        Self {
            listen_addr,
            external_node_addr,
            override_incoming_node_addr,
        }
    }
}

impl RemoteServer {
    pub fn new() -> Self {
        RemoteServer {
            cancellation_token: CancellationToken::new(),
        }
    }

    pub async fn start(
        &mut self,
        config: RemoteServerConfig,
        system: RemoteActorSystem,
    ) -> Result<(), tokio::io::Error> {
        debug!(
            "starting remote server (node_id={}), config: {:#?}",
            system.node_id(),
            &config
        );

        let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;

        let session_store = RemoteSessionStore::new()
            .into_actor(Some("remote-session-store"), &system.actor_system())
            .await
            .unwrap();

        let remote_server_config = Arc::new(config);
        tokio::spawn(server_loop(
            listener,
            session_store,
            self.cancellation_token.clone(),
            remote_server_config,
        ));

        Ok(())
    }

    pub fn stop(&self) {
        self.cancellation_token.cancel();
    }
}

pub async fn cancellation(cancellation_token: CancellationToken) {
    cancellation_token.cancelled().await
}

pub async fn accept(
    listener: &tokio::net::TcpListener,
    cancellation_token: CancellationToken,
) -> Option<tokio::io::Result<(tokio::net::TcpStream, SocketAddr)>> {
    tokio::select! {
        _ = cancellation(cancellation_token) => {
            None
        }

        res = listener.accept() => {
            Some(res)
        }
    }
}

pub async fn server_loop(
    listener: tokio::net::TcpListener,
    session_store: LocalActorRef<RemoteSessionStore>,
    cancellation_token: CancellationToken,
    remote_server_config: RemoteServerConfigRef,
) {
    loop {
        match accept(&listener, cancellation_token.clone()).await {
            Some(Ok((stream, addr))) => {
                let remote_server_config = remote_server_config.clone();

                let session_id = uuid::Uuid::new_v4();
                trace!("client accepted {}, session_id={}", addr, session_id);

                let session = session_store
                    .send(NewSession(RemoteSession::new(
                        session_id,
                        addr,
                        stream,
                        remote_server_config,
                    )))
                    .await;

                if let Err(e) = session {
                    error!(
                        "error creating session actor (session_id={}, addr={}), error: {:?}",
                        session_id, addr, e
                    );
                }
            }
            Some(Err(e)) => error!("error accepting client: {:?}", e),
            None => break,
        }
    }

    info!("tcp listener {:?} stopped", &listener)
}
