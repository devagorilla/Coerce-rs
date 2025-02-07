# Coerce-rs  ![coerce-rs](https://github.com/LeonHartley/Coerce-rs/workflows/coerce-rs%20tests/badge.svg)

Coerce-rs is an asynchronous (async/await) Actor runtime and distributed system framework for Rust. It allows for
extremely simple yet powerful actor-based distributed system development. With minimal code, you can build a highly
scalable, fault-tolerant modern actor-driven application.

## Crates
| **Crate**                                               | **Purpose**                                                                                                                           | **Latest Version**                                              |
|---------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------|
| [coerce](https://crates.io/crates/coerce)               | The main Coerce runtime and framework                                                                                                 | ![crates.io](https://img.shields.io/crates/v/coerce.svg)        |
| [coerce-redis](https://crates.io/crates/coerce-redis)   | Actor persistence provider for Redis. Enables event source and snapshots to be read and written from Redis.                           | ![crates.io](https://img.shields.io/crates/v/coerce-redis.svg)  |
| [coerce-macros](https://crates.io/crates/coerce-macros) | Useful macros allowing for quick implementations of snapshots, JSON-serialisable remote messages and more.                            | ![crates.io](https://img.shields.io/crates/v/coerce-macros.svg) |
| [coerce-k8s](https://crates.io/crates/coerce-k8s)       | Kubernetes discovery provider, automatically discover cluster peers hosted in Kubernetes, based on a configurable pod-selection label | ![crates.io](https://img.shields.io/crates/v/coerce-k8s.svg)    |

# Using Coerce in your own project
First step to using Coerce in your project is to add the coerce crate dependency, this can be done by adding the following
to your Cargo.toml:

```toml
[dependencies]
coerce = { version = "0.8", features = ["full"] }
```

Coerce currently relies on an unstable feature from the `tracing` crate, `valuable`, used for enriching logs with
information on the actor context, which can be enabled by adding the following to your `.cargo/config.toml` file:
```
[build]
rustflags = ["--cfg", "tracing_unstable"]
```

## Features

### Actors
 - Type-safe actors
 - Supervision / child spawning
 - Location-transparent `ActorRef<A>` types (ActorRef may comprise of a `LocalActorRef<A>` or a `RemoteActorRef<A>`)
 - Metrics available out of the box

## Remoting
  - Communicate with an actor from anywhere in the cluster
  - Actors can be deployed locally or to other remote nodes
  - Protobuf network protocol
  - Actor-driven networking layer

### Distributed Sharding

- Actor IDs can resolve to specific shards, which can be spread across a cluster of Coerce nodes
- Automatic load balancing, shards will be fairly allocated across the cluster
- Self-recovering when nodes are lost, actors can be automatically restarted on other healthy nodes

### Persistence

- Journaling / event sourcing
- Snapshotting
- Pluggable storage providers (in-memory and redis readily available, MySQL is planned)

### Distributed PubSub

- Actors can subscribe to programmable topics from anywhere in the cluster
- System-level topic provided to receive updated system state (e.g new nodes joining, nodes lost etc.)

### HTTP API

- Easily accessible metrics and information useful for diagnosis


# Building and testing the Coerce libraries
Building Coerce is easy. All you need is the latest Rust stable or nightly installed, along with Cargo.

```shell
# Clone the repository
git clone https://github.com/leonhartley/coerce-rs && cd coerce-rs

## run Cargo build to build the entire workspace, including the examples and the tests
cargo build

## Alternatively, if you'd like to build the library, dependencies and run the tests
cargo test --all-features
```

# How to run the examples
### Sharded Chat example


# ActorSystem

Every actor belongs to an ActorSystem.

### async/await Actors

An actor is just another word for a unit of computation. It can have mutable state, it can receive messages and perform
actions. One caveat though.. It can only do one thing at a time. This can be useful because it can alleviate the need
for thread synchronisation, usually achieved by locking (using `Mutex`, `RwLock` etc).



#### How is this achieved in Coerce?

Coerce uses Tokio's MPSC channels ([tokio::sync::mpsc::channel][channel]), every actor created spawns a task listening
to messages from a
`Receiver`, handling and awaiting the result of the message. Every reference (`ActorRef<A: Actor>`) holds
a `Sender<M> where A: Handler<M>`, which can be cloned.

Actors can be stopped and actor references can be retrieved by ID from anywhere in your application. IDs are `String`
but if an ID isn't provided upon creation, a new `Uuid` will be generated. Anonymous actors are automatically dropped (
and `Stopped`)
when all references are dropped. Tracked actors (using global fn `new_actor`) must be stopped.

<details>
  <summary>Basic ActorSystem + EchoActor example</summary>

### Example

```rust
pub struct EchoActor {}

#[async_trait]
impl Actor for EchoActor {}

pub struct EchoMessage(String);

impl Message for EchoMessage {
    type Result = String;
}

#[async_trait]
impl Handler<EchoMessage> for EchoActor {
    async fn handle(
        &mut self,
        message: EchoMessage,
        _ctx: &mut ActorContext,
    ) -> String {
        message.0.clone()
    }
}

pub async fn run() {
    let mut actor = new_actor(EchoActor {}).await.unwrap();

    let hello_world = "hello, world".to_string();
    let result = actor.send(EchoMessage(hello_world.clone())).await;

    assert_eq!(result, Ok(hello_world));
}
```

### Timer Example

```rust
pub struct EchoActor {}

#[async_trait]
impl Actor for EchoActor {}

pub struct EchoMessage(String);

impl Message for EchoMessage {
    type Result = String;
}

pub struct PrintTimer(String);

impl TimerTick for PrintTimer {}

#[async_trait]
impl Handler<PrintTimer> for EchoActor {
    async fn handle(&mut self, msg: PrintTimer, _ctx: &mut ActorContext) {
        println!("{}", msg.0);
    }
}

pub async fn run() {
    let mut actor = new_actor(EchoActor {}).await.unwrap();
    let hello_world = "hello world!".to_string();

    // print "hello world!" every 5 seconds
    let timer = Timer::start(actor.clone(), Duration::from_secs(5), TimerTick(hello_world));

    // timer is stopped when handle is out of scope or can be stopped manually by calling `.stop()`
    sleep(Duration::from_secs(20)).await;
    timer.stop();
}
```

</details>

# RemoteActorSystem



[channel]: https://docs.rs/tokio/0.2.4/tokio/sync/mpsc/fn.channel.html
