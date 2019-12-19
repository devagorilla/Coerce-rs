use crate::actor::scheduler::{ActorScheduler, GetActor, RegisterActor};
use crate::actor::{Actor, ActorId, ActorRef, ActorRefError};

lazy_static! {
    static ref CURRENT_CONTEXT: ActorContext = { ActorContext::new() };
}

#[derive(Clone)]
pub struct ActorContext {
    scheduler: ActorRef<ActorScheduler>,
}

impl ActorContext {
    pub fn new() -> ActorContext {
        ActorContext {
            scheduler: ActorScheduler::new(),
        }
    }

    pub fn current_context() -> ActorContext {
        CURRENT_CONTEXT.clone()
    }

    pub async fn new_tracked_actor<A: Actor>(
        &mut self,
        actor: A,
    ) -> Result<ActorRef<A>, ActorRefError>
    where
        A: 'static + Sync + Send,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let actor_ref = self.scheduler.send(RegisterActor(actor, tx)).await;

        match rx.await {
            Ok(true) => actor_ref,
            _ => Err(ActorRefError::ActorUnavailable),
        }
    }

    #[deprecated = "replaced by new_tracked_actor, to create untracked - use coerce_rt::actor::new_actor()"]
    pub async fn new_actor<A: Actor>(&mut self, actor: A) -> Result<ActorRef<A>, ActorRefError>
    where
        A: 'static + Sync + Send,
    {
        self.new_tracked_actor(actor).await
    }

    pub async fn get_actor<A: Actor>(&mut self, id: ActorId) -> Option<ActorRef<A>>
    where
        A: 'static + Sync + Send,
    {
        match self.scheduler.send(GetActor::new(id)).await {
            Ok(a) => a,
            Err(_) => None,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum ActorStatus {
    Starting,
    Started,
    Stopping,
    Stopped,
}

pub struct ActorHandlerContext {
    status: ActorStatus,
}

impl ActorHandlerContext {
    pub fn new(status: ActorStatus) -> ActorHandlerContext {
        ActorHandlerContext { status }
    }

    pub fn set_status(&mut self, state: ActorStatus) {
        self.status = state
    }

    pub fn get_status(&self) -> &ActorStatus {
        &self.status
    }
}
