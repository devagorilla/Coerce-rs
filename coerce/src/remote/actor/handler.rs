use crate::actor::context::ActorContext;
use crate::actor::message::Handler;
use crate::remote::actor::message::{
    ClientWrite, DeregisterClient, GetActorNode, GetNodes, PopRequest, PushRequest, RegisterActor,
    RegisterClient, RegisterNode, RegisterNodes, SetRemote, UpdateNodes,
};
use crate::remote::actor::{
    RemoteClientRegistry, RemoteHandler, RemoteRegistry, RemoteRequest, RemoteResponse,
};
use crate::remote::cluster::node::{RemoteNode, RemoteNodeState};
use crate::remote::net::client::{ClientType, RemoteClient};
use crate::remote::system::{NodeId, RemoteActorSystem};

use std::collections::HashMap;
use std::sync::Arc;

use crate::remote::net::message::SessionEvent;
use crate::remote::net::proto::network::{ActorAddress, FindActor};

use crate::actor::{Actor, ActorId, LocalActorRef};
use crate::remote::stream::pubsub::{PubSub, StreamEvent};
use crate::remote::stream::system::{ClusterEvent, SystemEvent, SystemTopic};
use crate::remote::tracing::extract_trace_identifier;

use crate::remote::net::client::connect::Connect;
use crate::remote::net::client::send::Write;
use crate::remote::net::client::ClientType::Worker;
use protobuf::Message;
use std::time::Instant;
use uuid::Uuid;

#[async_trait]
impl Handler<SetRemote> for RemoteRegistry {
    async fn handle(&mut self, message: SetRemote, ctx: &mut ActorContext) {
        let sys = message.0;
        ctx.set_system(sys.actor_system().clone());
        self.system = Some(sys);

        let subscription = PubSub::subscribe::<Self, SystemTopic>(SystemTopic, ctx).await;
        if let Ok(subscription) = subscription {
            trace!(target: "RemoteRegistry", "subscribed to system event");
            self.system_event_subscription = Some(subscription);
        }
    }
}

#[async_trait]
impl Handler<GetNodes> for RemoteRegistry {
    async fn handle(
        &mut self,
        _message: GetNodes,
        _ctx: &mut ActorContext,
    ) -> Vec<RemoteNodeState> {
        self.nodes.get_all()
    }
}

#[async_trait]
impl Handler<PushRequest> for RemoteHandler {
    async fn handle(&mut self, message: PushRequest, _ctx: &mut ActorContext) {
        self.requests.insert(message.0, message.1);
    }
}

#[async_trait]
impl Handler<PopRequest> for RemoteHandler {
    async fn handle(
        &mut self,
        message: PopRequest,
        _ctx: &mut ActorContext,
    ) -> Option<RemoteRequest> {
        self.requests.remove(&message.0)
    }
}

#[async_trait]
impl Handler<RegisterClient> for RemoteClientRegistry {
    async fn handle(&mut self, message: RegisterClient, _ctx: &mut ActorContext) {
        self.add_client(message.0, message.1);

        trace!(target: "RemoteRegistry", "client {} registered", message.0);
    }
}

#[async_trait]
impl Handler<RegisterNodes> for RemoteRegistry {
    async fn handle(&mut self, message: RegisterNodes, _ctx: &mut ActorContext) {
        let remote = self.system.as_ref().unwrap().clone();
        let nodes = message.0;

        let unregistered_nodes = nodes
            .iter()
            .filter(|node| node.id != remote.node_id() && !self.nodes.is_registered(node.id))
            .map(|node| node.clone())
            .collect::<Vec<RemoteNode>>();

        trace!(target: "RemoteRegistry", "registering new nodes {:?}", &unregistered_nodes);

        let current_nodes = self.nodes.get_all();

        if !unregistered_nodes.is_empty() {
            let connected_nodes =
                connect_all(unregistered_nodes, current_nodes, remote.clone()).await;
            for connected_node in connected_nodes {
                self.register_node(connected_node);
            }
        }

        for node in nodes {
            let sys = remote.clone();
            let node_id = node.id;
            tokio::spawn(async move {
                let sys = sys;
                PubSub::publish_locally(
                    SystemTopic,
                    SystemEvent::Cluster(ClusterEvent::NodeAdded(node_id)),
                    sys.actor_system().remote(),
                )
                .await;
            });

            self.nodes.add(node);
        }
    }
}

#[async_trait]
impl Handler<RegisterNode> for RemoteRegistry {
    async fn handle(&mut self, message: RegisterNode, ctx: &mut ActorContext) {
        if ctx.system().is_remote() {
            PubSub::publish_locally(
                SystemTopic,
                SystemEvent::Cluster(ClusterEvent::NodeAdded(message.0.id)),
                self.system.as_ref().unwrap(),
            )
                .await;
        }

        self.register_node(message.0);
    }
}

impl RemoteRegistry {
    pub fn register_node(&mut self, node: RemoteNode) {
        self.nodes.add(node);
    }
}

#[async_trait]
impl Handler<UpdateNodes> for RemoteRegistry {
    async fn handle(&mut self, message: UpdateNodes, _ctx: &mut ActorContext) {
        self.nodes.update_nodes(message.0);
    }
}

#[async_trait]
impl Handler<ClientWrite> for RemoteClientRegistry {
    async fn handle(&mut self, message: ClientWrite, _ctx: &mut ActorContext) {
        let client_id = message.0;
        let message = message.1;

        // TODO: we could open multiple clients per node and use some routing mechanism
        //       to potentially improve throughput, whilst still maintaining
        //       message ordering

        if let Some(client) = self.clients.get_mut(&client_id) {
            client.send(Write(message)).await.expect("send client msg");
            trace!(target: "RemoteRegistry", "writing data to client")
        } else {
            trace!(target: "RemoteRegistry", "client {} not found", &client_id);
        }
    }
}

#[async_trait]
impl Handler<DeregisterClient> for RemoteClientRegistry {
    async fn handle(&mut self, message: DeregisterClient, _ctx: &mut ActorContext) {
        let node_id = message.0;
        self.remove_client(node_id);
        trace!(target: "RemoteRegistry", "removing client {}", &node_id);
    }
}

#[async_trait]
impl Handler<GetActorNode> for RemoteRegistry {
    async fn handle(&mut self, message: GetActorNode, _: &mut ActorContext) {
        let span = tracing::trace_span!(
            "RemoteRegistry::GetActorNode",
            actor_id = message.actor_id.as_str()
        );
        let _enter = span.enter();

        let id = message.actor_id;
        let current_system = self.system.as_ref().unwrap().node_id();
        let assigned_registry_node = self.nodes.get_by_key(&id).map(|n| n.id);

        let assigned_registry_node = assigned_registry_node.map_or_else(
            || {
                trace!(target: "RemoteRegistry", "no nodes configured, assigning locally");
                current_system
            },
            |n| n,
        );

        trace!(target: "RemoteRegistry", "{:?}", &self.nodes.get_all());

        let local_registry_entry = self.actors.get(&id);
        if local_registry_entry.is_some() || &assigned_registry_node == &current_system {
            trace!(target: "RemoteRegistry::GetActorNode", "searching locally, {}", current_system);
            let node = local_registry_entry.map(|s| *s);

            trace!(target: "RemoteRegistry::GetActorNode", "found: {:?}", &node);
            message.sender.send(node);
        } else {
            let system = self.system.as_ref().unwrap().clone();
            let sender = message.sender;

            trace!(target: "RemoteRegistry::GetActorNode", "asking remotely, current_sys={}, target_sys={}", current_system, assigned_registry_node);
            tokio::spawn(async move {
                let span = tracing::trace_span!("RemoteRegistry::GetActorNode::Remote");
                let _enter = span.enter();

                let message_id = Uuid::new_v4();
                let system = system;
                let (res_tx, res_rx) = tokio::sync::oneshot::channel();

                trace!(target: "RemoteRegistry::GetActorNode", "remote request={}", message_id);
                system.push_request(message_id, res_tx);

                trace!(target: "RemoteRegistry::GetActorNode", "sending actor lookup request to={}", assigned_registry_node);
                let trace_id = extract_trace_identifier(&span);
                system
                    .send_message(
                        assigned_registry_node,
                        SessionEvent::FindActor(FindActor {
                            message_id: message_id.to_string(),
                            actor_id: id,
                            trace_id,
                            ..FindActor::default()
                        }),
                    )
                    .await;

                trace!(target: "RemoteRegistry::GetActorNode", "lookup sent, waiting for result");
                match res_rx.await {
                    Ok(RemoteResponse::Ok(res)) => {
                        let res = ActorAddress::parse_from_bytes(&res);
                        match res {
                            Ok(res) => {
                                sender.send(if res.get_node_id() == 0 {
                                    None
                                } else {
                                    Some(res.get_node_id())
                                });
                            }
                            Err(e) => {
                                panic!("failed to decode message - {}", e.to_string());
                            }
                        }
                    }
                    _ => panic!("get actornode failed"),
                }
            });
        }
    }
}

#[async_trait]
impl Handler<RegisterActor> for RemoteRegistry {
    async fn handle(&mut self, message: RegisterActor, _ctx: &mut ActorContext) {
        trace!(target: "RemoteRegistry::RegisterActor", "Registering actor: {:?}, node={}", &message, self.system.as_ref().unwrap().node_id());

        match message.node_id {
            Some(node_id) => {
                trace!(target: "RemoteRegistry", "registering actor locally {}", node_id);
                self.actors.insert(message.actor_id, node_id);
            }

            None => {
                if let Some(system) = self.system.as_mut() {
                    let node_id = system.node_id();
                    let id = message.actor_id;

                    let assigned_registry_node =
                        self.nodes.get_by_key(&id).map_or_else(|| node_id, |n| n.id);

                    if &assigned_registry_node == &node_id {
                        trace!("registering actor locally {}", assigned_registry_node);
                        self.actors.insert(id, node_id);
                    } else {
                        let system = system.clone();
                        tokio::spawn(async move {
                            let event = SessionEvent::RegisterActor(ActorAddress {
                                node_id,
                                actor_id: id,
                                ..ActorAddress::default()
                            });
                            system.send_message(assigned_registry_node, event).await;
                        });
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Handler<StreamEvent<SystemTopic>> for RemoteRegistry {
    async fn handle(&mut self, event: StreamEvent<SystemTopic>, ctx: &mut ActorContext) {
        match event {
            StreamEvent::Receive(msg) => match msg.as_ref() {
                SystemEvent::Cluster(_) => {
                    trace!(target: "RemoteRegistry", "cluster event");
                    let system = self.system.as_ref().unwrap().clone();
                    let registry_ref = self.actor_ref(ctx);

                    tokio::spawn(async move {
                        let sys = system;
                        let actor_ids = sys
                            .actor_system()
                            .scheduler()
                            .exec::<_, Vec<ActorId>>(|s| {
                                s.actors.keys().map(|k| k.clone()).collect()
                            })
                            .await
                            .expect("unable to get active actor ids from scheduler");

                        for actor_id in actor_ids {
                            registry_ref.notify(RegisterActor::new(actor_id, None));
                        }
                    });
                }
            },
            StreamEvent::Err => {
                warn!(target: "RemoteRegistry", "received stream err");
            }
        }
    }
}

async fn connect_all(
    nodes: Vec<RemoteNode>,
    current_nodes: Vec<RemoteNodeState>,
    system: RemoteActorSystem,
) -> Vec<RemoteNode> {
    debug!(
        "discovered {} new nodes, currently active peers={}",
        nodes.len(),
        current_nodes.len()
    );

    let mut connected_nodes = vec![];
    for node in nodes {
        let addr = node.addr.to_string();
        match RemoteClient::new(addr, Some(node.id), system.clone(), Worker, false).await {
            Ok(client) => {
                trace!(target: "RemoteRegistry", "connecting to node_id={}, addr={}", node.id, node.addr);
                if let Some(node) = client
                    .send(Connect::new(Some(current_nodes.clone())))
                    .await
                    .unwrap()
                {
                    connected_nodes.push(node);
                } else {
                    warn!(target: "RemoteRegistry", "failed to node_id={}, addr={}", node.id, node.addr);
                }
            }
            Err(_) => {
                warn!(target: "RemoteRegistry", "failed to create remoteclient actor for remote node (node_id={}, addr={})", node.id, node.addr);
            }
        }
    }

    connected_nodes
}
