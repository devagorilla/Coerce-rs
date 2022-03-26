use chrono::Utc;
use protobuf::{Message as ProtoMessage, ProtobufResult};
use std::time::Instant;
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::RecvError;
use tokio::time::error::Elapsed;
use tokio::time::timeout;
use uuid::Uuid;

use crate::actor::context::ActorContext;
use crate::actor::message::{Handler, Message};
use crate::actor::scheduler::timer::TimerTick;
use crate::remote::actor::RemoteResponse;
use crate::remote::cluster::node::RemoteNodeState;
use crate::remote::heartbeat::{NodePing, PingResult};
use crate::remote::net::client::{ClientState, RemoteClient};
use crate::remote::net::message::SessionEvent;
use crate::remote::net::proto::network::{Ping, Pong};

#[derive(Clone)]
pub struct PingTick;

impl Message for PingTick {
    type Result = ();
}

impl TimerTick for PingTick {}

#[async_trait]
impl Handler<PingTick> for RemoteClient {
    async fn handle(&mut self, _: PingTick, ctx: &mut ActorContext) {
        let remote = ctx.system().remote_owned();
        let heartbeat = if remote.heartbeat_actor().is_none() {
            // No heartbeat actor available, no need to ping
            trace!("skipping ping tick, no heartbeat actor");
            return;
        } else {
            remote.heartbeat_actor().unwrap().clone()
        };

        let node_id = if let Some(state) = &self.state {
            match state {
                ClientState::Connected(state) => state.node_id,
                _ => {
                    if let Some(node_id) = self.remote_node_id {
                        let _ = heartbeat.notify(NodePing(node_id, PingResult::Disconnected));
                    }

                    return;
                }
            }
        } else {
            return;
        };

        let (res_tx, res_rx) = oneshot::channel();
        let message_id = Uuid::new_v4();
        remote.push_request(message_id, res_tx);

        let ping_event = SessionEvent::Ping(Ping {
            message_id: message_id.to_string(),
            ..Ping::default()
        });

        let ping_start = Instant::now();
        if self.write(ping_event, ctx).await.is_ok() {
            tokio::spawn(async move {
                let timeout = remote.config().heartbeat_config().ping_timeout;

                let ping_result_receiver = res_rx;
                let ping_result = match tokio::time::timeout(timeout, ping_result_receiver).await {
                    Ok(res) => match res {
                        Ok(pong) => match pong {
                            RemoteResponse::Ok(pong_bytes) => {
                                let ping_end = ping_start.elapsed();
                                let pong = Pong::parse_from_bytes(&pong_bytes).unwrap();

                                PingResult::Ok(pong, ping_end, Utc::now())
                            }
                            RemoteResponse::Err(_err_bytes) => PingResult::Err,
                        },
                        Err(_e) => PingResult::Err,
                    },
                    Err(_e) => PingResult::Timeout,
                };
                let _ = heartbeat.notify(NodePing(node_id, ping_result));
            });
        } else {
            warn!("(addr={}) ping write failed", &self.addr);
        }
    }
}
