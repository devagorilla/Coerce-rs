use crate::actor::context::ActorContext;
use crate::actor::message::{Envelope, Handler, Message, MessageUnwrapErr, MessageWrapErr};
use crate::actor::{Actor, ActorId, ActorRef, ActorRefErr};
use crate::remote::cluster::sharding::coordinator::allocation::{
    AllocateShard, AllocateShardResult,
};

use crate::remote::cluster::sharding::host::{ShardAllocated, ShardHost, ShardState};
use crate::remote::cluster::sharding::proto::sharding as proto;
use crate::remote::cluster::sharding::shard::Shard;
use crate::remote::system::{NodeId, RemoteActorSystem};

use protobuf::{Message as ProtoMessage, SingularPtrField};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::oneshot::{channel, Sender};
use uuid::Uuid;

pub struct EntityRequest {
    pub actor_id: ActorId,
    pub message_type: String,
    pub message: Vec<u8>,
    pub recipe: Option<Arc<Vec<u8>>>,
    pub result_channel: Option<Sender<Result<Vec<u8>, ActorRefErr>>>,
}

pub struct RemoteEntityRequest {
    pub request_id: Uuid,
    pub actor_id: ActorId,
    pub message_type: String,
    pub message: Vec<u8>,
    pub recipe: Option<Vec<u8>>,
    pub origin_node: NodeId,
}

impl ShardHost {
    pub fn handle_request(&self, message: EntityRequest, shard_state: &mut ShardState) {
        match shard_state {
            ShardState::Starting { request_buffer } => request_buffer.push(message),

            ShardState::Ready(actor) => {
                let actor = actor.clone();
                tokio::spawn(async move {
                    let actor_id = message.actor_id.clone();
                    let message_type = message.message_type.clone();

                    let result = actor.send(message).await;
                    if !result.is_ok() {
                        error!(
                            "failed to deliver EntityRequest (actor_id={}, type={}) to shard (shard_id={})",
                            &actor_id, &message_type, shard_id
                        );
                    } else {
                        trace!(
                            "delivered EntityRequest (actor_id={}, type={}) to shard (shard_id={})",
                            &actor_id,
                            message_type,
                            shard_id
                        );
                    }
                });
            }
        }
    }
}

#[async_trait]
impl Handler<EntityRequest> for ShardHost {
    async fn handle(&mut self, message: EntityRequest, ctx: &mut ActorContext) {
        let shard_id = self.allocator.allocate(&message.actor_id);

        if let Some(shard) = self.hosted_shards.get_mut(&shard_id) {
            self.handle_request(message, shard);
        } else if let Some(shard) = self.remote_shards.get(&shard_id) {
            let shard_ref = shard.clone();
            tokio::spawn(remote_entity_request(
                shard_ref,
                message,
                ctx.system().remote_owned(),
            ));
        } else if ctx.system().remote().current_leader().is_some() {
            let leader = self.get_coordinator(&ctx).await;

            let buffered_requests = self.requests_pending_shard_allocation.entry(shard_id);
            let buffered_requests = buffered_requests.or_insert_with(|| vec![]);
            buffered_requests.push(message);

            debug!("shard#{} not allocated, notifying coordinator and buffering request (buffered_requests={})", shard_id, buffered_requests.len());

            let host_ref = self.actor_ref(ctx);
            tokio::spawn(async move {
                let allocation = leader.send(AllocateShard { shard_id }).await;
                if let Ok(AllocateShardResult::Allocated(shard_id, node_id)) = allocation {
                    host_ref.notify(ShardAllocated(shard_id, node_id));
                }
            });
        } else {
            self.requests_pending_leader_allocation.push_back(message);

            debug!(
                "no leader allocated, buffering message (requests_pending_leader_allocation={})",
                self.requests_pending_leader_allocation.len()
            );
        }
    }
}

async fn remote_entity_request(
    shard_ref: ActorRef<Shard>,
    mut request: EntityRequest,
    system: RemoteActorSystem,
) -> Result<(), ActorRefErr> {
    let request_id = Uuid::new_v4();
    let (tx, rx) = channel();
    system.push_request(request_id, tx);

    let result_channel = request.result_channel.take();

    trace!(
        "emitting RemoteEntityRequest to node_id={} from node_id={}",
        shard_ref.node_id().unwrap(),
        system.node_id()
    );

    shard_ref
        .notify(RemoteEntityRequest {
            origin_node: system.node_id(),
            request_id,
            actor_id: request.actor_id,
            message_type: request.message_type,
            message: request.message,
            recipe: request.recipe.map(|r| r.as_ref().clone()),
        })
        .await
        .expect("shard notify");

    trace!(
        "emitted RemoteEntityRequest to node_id={} from node_id={}, waiting for reply",
        shard_ref.node_id().unwrap(),
        system.node_id()
    );

    let res = match rx.await {
        Ok(response) => {
            result_channel.map(move |result_sender| {
                let response = response
                    .into_result()
                    .map_err(|_| ActorRefErr::ActorUnavailable);

                result_sender.send(response)
            });

            Ok(())
        }
        Err(_) => Err(ActorRefErr::ActorUnavailable),
    };

    trace!(
        "received reply for RemoteEntityRequest from node_id={} to node_id={}",
        system.node_id(),
        shard_ref.node_id().unwrap()
    );

    res
}

impl From<RemoteEntityRequest> for EntityRequest {
    fn from(req: RemoteEntityRequest) -> Self {
        EntityRequest {
            actor_id: req.actor_id,
            message_type: req.message_type,
            message: req.message,
            recipe: req.recipe.map(|r| Arc::new(r)),
            result_channel: None,
        }
    }
}

impl Message for EntityRequest {
    type Result = ();
}

impl Message for RemoteEntityRequest {
    type Result = ();

    fn as_remote_envelope(&self) -> Result<Envelope<Self>, MessageWrapErr> {
        let proto = proto::RemoteEntityRequest {
            request_id: self.request_id.to_string(),
            actor_id: self.actor_id.clone(),
            message_type: self.message_type.clone(),
            message: self.message.clone(),
            recipe: self.recipe.clone().map_or_else(
                || SingularPtrField::none(),
                |r| {
                    Some(proto::RemoteEntityRequest_Recipe {
                        recipe: r,
                        ..Default::default()
                    })
                    .into()
                },
            ),
            origin_node: self.origin_node,
            ..Default::default()
        };

        proto.write_to_bytes().map_or_else(
            |_e| Err(MessageWrapErr::SerializationErr),
            |bytes| Ok(Envelope::Remote(bytes)),
        )
    }

    fn from_remote_envelope(buffer: Vec<u8>) -> Result<Self, MessageUnwrapErr> {
        proto::RemoteEntityRequest::parse_from_bytes(&buffer).map_or_else(
            |_e| Err(MessageUnwrapErr::DeserializationErr),
            |proto| {
                Ok(RemoteEntityRequest {
                    request_id: Uuid::from_str(&proto.request_id).unwrap(),
                    actor_id: proto.actor_id,
                    message_type: proto.message_type,
                    message: proto.message,
                    recipe: proto
                        .recipe
                        .into_option()
                        .map_or(None, |recipe| Some(recipe.recipe)),
                    origin_node: proto.origin_node,
                })
            },
        )
    }

    fn read_remote_result(_buffer: Vec<u8>) -> Result<Self::Result, MessageUnwrapErr> {
        Ok(())
    }

    fn write_remote_result(_res: Self::Result) -> Result<Vec<u8>, MessageWrapErr> {
        Ok(vec![])
    }
}
