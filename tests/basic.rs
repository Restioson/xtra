use std::cmp::Ordering as CmpOrdering;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use futures_util::task::noop_waker_ref;
use futures_util::FutureExt;
use smol::stream;
use smol_timeout::TimeoutExt;
use xtra::prelude::*;
use xtra::spawn::TokioGlobalSpawnExt;
use xtra::KeepRunning;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Accumulator(usize);

#[async_trait]
impl Actor for Accumulator {
    type Stop = usize;

    async fn stopped(self) -> usize {
        self.0
    }
}

struct Inc;

struct Report;

#[async_trait]
impl Handler<Inc> for Accumulator {
    type Return = ();

    async fn handle(&mut self, _: Inc, _ctx: &mut Context<Self>) {
        self.0 += 1;
    }
}

#[async_trait]
impl Handler<Report> for Accumulator {
    type Return = Self;

    async fn handle(&mut self, _: Report, _ctx: &mut Context<Self>) -> Self {
        self.clone()
    }
}

#[async_trait]
impl Handler<StopAll> for Accumulator {
    type Return = ();

    async fn handle(&mut self, _: StopAll, ctx: &mut Context<Self>) -> Self::Return {
        ctx.stop_all();
    }
}

#[async_trait]
impl Handler<StopSelf> for Accumulator {
    type Return = ();

    async fn handle(&mut self, _: StopSelf, ctx: &mut Context<Self>) {
        ctx.stop_self();
    }
}

#[tokio::test]
async fn accumulate_to_ten() {
    let (addr, fut) = Accumulator(0).create(None).run();
    tokio::spawn(fut);
    for _ in 0..10 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    assert_eq!(addr.send(Report).await.unwrap().0, 10);
}

struct DropTester(Arc<AtomicUsize>);

impl Drop for DropTester {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Actor for DropTester {
    type Stop = ();

    async fn stopped(self) {
        self.0.fetch_add(5, Ordering::SeqCst);
    }
}

struct StopAll;

struct StopSelf;

#[async_trait]
impl Handler<StopSelf> for DropTester {
    type Return = ();

    async fn handle(&mut self, _: StopSelf, ctx: &mut Context<Self>) {
        ctx.stop_self();
    }
}

#[tokio::test]
async fn test_stop_and_drop() {
    // Drop the address
    let drop_count = Arc::new(AtomicUsize::new(0));
    let (addr, fut) = DropTester(drop_count.clone()).create(None).run();
    let handle = tokio::spawn(fut);
    let weak = addr.downgrade();
    let join = weak.join();
    drop(addr);
    handle.await.unwrap();
    assert_eq!(drop_count.load(Ordering::SeqCst), 1 + 5);
    assert!(!weak.is_connected());
    assert!(join.now_or_never().is_some());

    // Send a stop message
    let drop_count = Arc::new(AtomicUsize::new(0));
    let (addr, fut) = DropTester(drop_count.clone()).create(None).run();
    let handle = tokio::spawn(fut);
    let weak = addr.downgrade();
    let join = weak.join();
    let _ = addr.send(StopSelf).split_receiver().await;
    handle.await.unwrap();
    assert_eq!(drop_count.load(Ordering::SeqCst), 1 + 5);
    assert!(!weak.is_connected());
    assert!(join.now_or_never().is_some());

    // Drop address before future has even begun
    let drop_count = Arc::new(AtomicUsize::new(0));
    let (addr, fut) = DropTester(drop_count.clone()).create(None).run();
    let weak = addr.downgrade();
    let join = weak.join();
    drop(addr);
    tokio::spawn(fut).await.unwrap();
    assert_eq!(drop_count.load(Ordering::SeqCst), 1 + 5);
    assert!(!weak.is_connected());
    assert!(join.now_or_never().is_some());

    // Send a stop message before future has even begun
    let drop_count = Arc::new(AtomicUsize::new(0));
    let (addr, fut) = DropTester(drop_count.clone()).create(None).run();
    let weak = addr.downgrade();
    let join = weak.join();
    let _ = addr.send(StopSelf).split_receiver().await;
    tokio::spawn(fut).await.unwrap();
    assert_eq!(drop_count.load(Ordering::SeqCst), 1 + 5);
    assert!(!weak.is_connected());
    assert!(join.now_or_never().is_some());
}

#[tokio::test]
async fn handle_left_messages() {
    let (addr, fut) = Accumulator(0).create(None).run();

    for _ in 0..10 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    drop(addr);

    assert_eq!(fut.await, 10);
}

#[tokio::test]
async fn do_not_handle_drained_messages() {
    let (addr, fut) = Accumulator(0).create(None).run();

    for _ in 0..5 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    let _ = addr.send(StopSelf).split_receiver().await;

    for _ in 0..5 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    assert_eq!(fut.await, 5);

    let (addr, mut ctx) = Context::new(None);
    let fut1 = ctx.attach(Accumulator(0));
    let fut2 = ctx.run(Accumulator(0));

    for _ in 0..5 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    let _ = addr.send(StopAll).split_receiver().await;

    assert_eq!(fut1.await, 5);
    assert!(!addr.is_connected());

    for _ in 0..5 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    assert_eq!(fut2.await, 0);
    assert!(!addr.is_connected());
}

struct StreamCancelMessage;

struct StreamCancelTester;

#[async_trait]
impl Actor for StreamCancelTester {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[async_trait]
impl Handler<StreamCancelMessage> for StreamCancelTester {
    type Return = KeepRunning;

    async fn handle(&mut self, _: StreamCancelMessage, _: &mut Context<Self>) -> KeepRunning {
        KeepRunning::Yes
    }
}

#[tokio::test]
async fn test_stream_cancel_join() {
    let addr = StreamCancelTester {}.create(None).spawn_global();
    let jh = addr.join();
    let addr2 = addr.clone().downgrade();
    // attach a stream that blocks forever
    let handle = tokio::spawn(async move {
        addr2
            .attach_stream(stream::pending::<StreamCancelMessage>())
            .await
    });
    drop(addr); // stop the actor

    // Attach stream should return immediately
    assert!(handle.timeout(Duration::from_secs(2)).await.is_some()); // None == timeout

    // Join should also return right away
    assert!(jh.timeout(Duration::from_secs(2)).await.is_some());
}

#[tokio::test]
async fn single_actor_on_address_with_stop_self_returns_disconnected_on_stop() {
    let (address, fut) = ActorStopSelf.create(None).run();
    let _ = address.send(StopSelf).split_receiver().await;
    assert!(fut.now_or_never().is_some());
    assert!(address.join().now_or_never().is_some());
    assert!(!address.is_connected());
}

#[tokio::test]
async fn two_actors_on_address_with_stop_self() {
    let (address, mut ctx) = Context::new(None);
    tokio::spawn(ctx.attach(ActorStopSelf));
    tokio::spawn(ctx.run(ActorStopSelf));
    address.send(StopSelf).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(address.is_connected());
    address.send(StopSelf).await.unwrap();

    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!address.is_connected());
}

#[tokio::test]
async fn two_actors_on_address_with_stop_self_context_alive() {
    let (address, mut ctx) = Context::new(None);
    tokio::spawn(ctx.attach(ActorStopSelf));
    tokio::spawn(ctx.attach(ActorStopSelf)); // Context not dropped here
    address.send(StopSelf).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(address.is_connected());
    address.send(StopSelf).await.unwrap();

    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(address.is_connected());
}

struct ActorStopSelf;

#[async_trait]
impl Actor for ActorStopSelf {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {
        println!("Stopped");
    }
}

#[async_trait]
impl Handler<StopSelf> for ActorStopSelf {
    type Return = ();

    async fn handle(&mut self, _: StopSelf, ctx: &mut Context<Self>) {
        ctx.stop_self();
    }
}

struct LongRunningHandler;

#[async_trait]
impl Actor for LongRunningHandler {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[async_trait]
impl Handler<Duration> for LongRunningHandler {
    type Return = ();

    async fn handle(&mut self, duration: Duration, _: &mut Context<Self>) -> Self::Return {
        tokio::time::sleep(duration).await
    }
}

#[tokio::test]
async fn receiving_async_on_address_returns_immediately_after_dispatch() {
    let address = LongRunningHandler.create(None).spawn_global();

    let send_future = address.send(Duration::from_secs(3)).split_receiver();
    let handler_future = send_future
        .now_or_never()
        .expect("Dispatch should be immediate on first poll");

    handler_future.await.unwrap();
}

#[tokio::test]
async fn receiving_async_on_message_channel_returns_immediately_after_dispatch() {
    let address = LongRunningHandler.create(None).spawn_global();
    let channel = StrongMessageChannel::clone_channel(&address);

    let send_future = channel.send(Duration::from_secs(3)).split_receiver();
    let handler_future = send_future
        .now_or_never()
        .expect("Dispatch should be immediate on first poll");

    handler_future.await.unwrap();
}

#[derive(Eq, PartialEq, Clone, Debug)]
enum Message {
    Broadcast { priority: u32 },
    Priority { priority: u32 },
    Ordered { ord: u32 },
}

impl PartialOrd<Self> for Message {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for Message {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        match self {
            Message::Broadcast { priority } => match other {
                Message::Broadcast { priority: other } => priority.cmp(other),
                Message::Priority { priority: other } => priority.cmp(other),
                Message::Ordered { .. } => priority.cmp(&0),
            },
            Message::Priority { priority } => match other {
                Message::Broadcast { priority: other } => priority.cmp(other),
                Message::Priority { priority: other } => priority.cmp(other),
                Message::Ordered { .. } => priority.cmp(&0),
            },
            Message::Ordered { ord } => match other {
                Message::Broadcast { priority: other } => 0.cmp(other),
                Message::Priority { priority: other } => 0.cmp(other),
                Message::Ordered { ord: other } => other.cmp(ord),
            },
        }
    }
}

// An elephant never forgets :)
struct Elephant {
    name: &'static str,
    msgs: Vec<Message>,
}

impl Default for Elephant {
    fn default() -> Self {
        Elephant {
            name: "Indlovu",
            msgs: vec![],
        }
    }
}

#[async_trait]
impl Actor for Elephant {
    type Stop = Vec<Message>;

    async fn stopped(self) -> Self::Stop {
        self.msgs
    }
}

#[async_trait]
impl Handler<Message> for Elephant {
    type Return = ();

    async fn handle(&mut self, message: Message, _ctx: &mut Context<Self>) {
        eprintln!("{} is handling {:?}", self.name, message);
        self.msgs.push(message);
    }
}

#[tokio::test]
async fn handle_order() {
    let mut expected = vec![];

    let fut = {
        let (ele, fut) = Elephant::default().create(None).run();

        let mut send = |msg: Message| {
            expected.push(msg.clone());

            async {
                match msg {
                    Message::Broadcast { priority } => {
                        let _ = ele.broadcast(msg).priority(priority).await;
                    }
                    Message::Priority { priority } => {
                        let _ = ele.send(msg).priority(priority).split_receiver().await;
                    }
                    Message::Ordered { .. } => {
                        let _ = ele.send(msg).split_receiver().await;
                    }
                }
            }
        };

        send(Message::Ordered { ord: 0 }).await;
        send(Message::Ordered { ord: 1 }).await;
        send(Message::Ordered { ord: 2 }).await;
        send(Message::Broadcast { priority: 2 }).await;
        send(Message::Broadcast { priority: 3 }).await;
        send(Message::Broadcast { priority: 1 }).await;
        send(Message::Priority { priority: 4 }).await;
        send(Message::Broadcast { priority: 5 }).await;

        fut
    };

    expected.sort();
    expected.reverse();

    assert_eq!(fut.await, expected);
}

#[tokio::test]
async fn waiting_sender_order() {
    let (addr, mut ctx) = Context::new(Some(1));
    let mut fut_ctx = std::task::Context::from_waker(noop_waker_ref());
    let mut ele = Elephant::default();

    // With ordered messages

    let _ = addr
        .send(Message::Ordered { ord: 0 })
        .split_receiver()
        .await;
    let mut first = addr.send(Message::Ordered { ord: 1 }).split_receiver();
    let mut second = addr.send(Message::Ordered { ord: 2 }).split_receiver();

    assert!(first.poll_unpin(&mut fut_ctx).is_pending());
    assert!(second.poll_unpin(&mut fut_ctx).is_pending());

    ctx.yield_once(&mut ele).await;

    assert!(second.poll_unpin(&mut fut_ctx).is_pending());
    assert!(first.poll_unpin(&mut fut_ctx).is_ready());

    // With priority

    let _ = addr
        .send(Message::Priority { priority: 1 })
        .priority(1)
        .split_receiver()
        .await;
    let mut lesser = addr
        .send(Message::Priority { priority: 1 })
        .priority(1)
        .split_receiver();
    let mut greater = addr
        .send(Message::Priority { priority: 2 })
        .priority(2)
        .split_receiver();

    assert!(lesser.poll_unpin(&mut fut_ctx).is_pending());
    assert!(greater.poll_unpin(&mut fut_ctx).is_pending());

    ctx.yield_once(&mut ele).await;

    assert!(lesser.poll_unpin(&mut fut_ctx).is_pending());
    assert!(greater.poll_unpin(&mut fut_ctx).is_ready());

    // With broadcast

    let _ = addr
        .broadcast(Message::Broadcast { priority: 3 })
        .priority(3)
        .await;
    let mut lesser = addr
        .broadcast(Message::Broadcast { priority: 4 })
        .priority(4)
        .boxed();
    let mut greater = addr
        .broadcast(Message::Broadcast { priority: 5 })
        .priority(5)
        .boxed();

    assert!(lesser.poll_unpin(&mut fut_ctx).is_pending());
    assert!(greater.poll_unpin(&mut fut_ctx).is_pending());

    ctx.yield_once(&mut ele).await;

    assert!(lesser.poll_unpin(&mut fut_ctx).is_pending());
    assert!(greater.poll_unpin(&mut fut_ctx).is_ready());
}

#[tokio::test]
async fn set_priority_msg_channel() {
    let fut = {
        let (addr, fut) = Elephant::default().create(None).run();

        let channel = &addr as &dyn MessageChannel<Message, Return = ()>;

        let _ = channel
            .send(Message::Ordered { ord: 0 })
            .split_receiver()
            .await;
        let _ = channel
            .send(Message::Priority { priority: 1 })
            .priority(1)
            .split_receiver()
            .await;
        let _ = channel
            .send(Message::Priority { priority: 2 })
            .split_receiver()
            .priority(2)
            .await;

        fut
    };

    assert_eq!(
        fut.await,
        vec![
            Message::Priority { priority: 2 },
            Message::Priority { priority: 1 },
            Message::Ordered { ord: 0 },
        ]
    );
}

#[tokio::test]
async fn broadcast_tail_advances_bound_1() {
    let (addr, mut ctx) = Context::new(Some(1));
    let mut fut_ctx = std::task::Context::from_waker(noop_waker_ref());
    let mut ngwevu = Elephant {
        name: "Ngwevu",
        msgs: vec![],
    };

    tokio::spawn(ctx.attach(Elephant::default()));

    let _ = addr
        .broadcast(Message::Broadcast { priority: 0 })
        .priority(0)
        .await;

    ctx.yield_once(&mut ngwevu).await;

    assert_eq!(
        addr.broadcast(Message::Broadcast { priority: 0 })
            .priority(0)
            .timeout(Duration::from_secs(2))
            .await,
        Some(Ok(())),
        "New broadcast message should be accepted when queue is empty",
    );

    let mut lesser = addr
        .broadcast(Message::Broadcast { priority: 1 })
        .priority(1)
        .boxed();
    let mut greater = addr
        .broadcast(Message::Broadcast { priority: 2 })
        .priority(2)
        .boxed();

    assert!(
        lesser.poll_unpin(&mut fut_ctx).is_pending(),
        "Queue is full - should wait"
    );
    assert!(
        greater.poll_unpin(&mut fut_ctx).is_pending(),
        "Queue is full - should wait"
    );

    // Will handle the broadcast of priority 0 from earlier
    ctx.yield_once(&mut ngwevu).await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(
        lesser.poll_unpin(&mut fut_ctx).is_pending(),
        "Low prio - should wait"
    );
    assert!(
        greater.poll_unpin(&mut fut_ctx).is_ready(),
        "High prio - shouldn't wait"
    );

    // Should handle greater
    ctx.yield_once(&mut ngwevu).await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(
        lesser.poll_unpin(&mut fut_ctx).is_ready(),
        "Low prio should be sent in now"
    );
}

#[tokio::test]
async fn broadcast_tail_advances_bound_2() {
    let (addr, mut ctx) = Context::new(Some(2));
    let mut ngwevu = Elephant {
        name: "Ngwevu",
        msgs: vec![],
    };

    tokio::spawn(ctx.attach(Elephant::default()));

    let _ = addr
        .broadcast(Message::Broadcast { priority: 0 })
        .priority(0)
        .await;
    let _ = addr
        .broadcast(Message::Broadcast { priority: 0 })
        .priority(0)
        .await;

    ctx.yield_once(&mut ngwevu).await;

    assert_eq!(
        addr.broadcast(Message::Broadcast { priority: 0 })
            .priority(0)
            .timeout(Duration::from_secs(2))
            .await,
        Some(Ok(())),
        "New broadcast message should be accepted when queue has space",
    );
}

#[tokio::test]
async fn broadcast_tail_does_not_advance_unless_both_handle() {
    let (addr, mut ctx) = Context::new(Some(2));
    let mut ngwevu = Elephant {
        name: "Ngwevu",
        msgs: vec![],
    };

    let _fut = ctx.attach(Elephant::default());

    let _ = addr
        .broadcast(Message::Broadcast { priority: 0 })
        .priority(0)
        .await;
    let _ = addr
        .broadcast(Message::Broadcast { priority: 0 })
        .priority(0)
        .await;

    ctx.yield_once(&mut ngwevu).await;

    assert_eq!(
        addr.broadcast(Message::Broadcast { priority: 0 }).priority(0).timeout(Duration::from_secs(2)).await,
        None,
        "New broadcast message should NOT be accepted since the other actor has not yet handled the message",
    );
}

struct Greeter;

#[async_trait]
impl Actor for Greeter {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

struct Hello(&'static str);

#[async_trait]
impl Handler<Hello> for Greeter {
    type Return = String;

    async fn handle(&mut self, Hello(name): Hello, _: &mut Context<Self>) -> Self::Return {
        format!("Hello {}", name)
    }
}

#[derive(Clone)]
struct BroadcastHello(&'static str);

#[async_trait]
impl Handler<BroadcastHello> for Greeter {
    type Return = ();

    async fn handle(
        &mut self,
        BroadcastHello(name): BroadcastHello,
        _: &mut Context<Self>,
    ) -> Self::Return {
        println!("Hello {}", name)
    }
}

#[tokio::test]
async fn address_send_exercises_backpressure() {
    let (address, mut context) = Context::new(Some(1));

    // Regular send

    let _ = address
        .send(Hello("world"))
        .split_receiver()
        .now_or_never()
        .expect("be able to queue 1 message because the mailbox is empty");
    let handler2 = address.send(Hello("world")).split_receiver().now_or_never();
    assert!(
        handler2.is_none(),
        "Fail to queue 2nd message because mailbox is full"
    );

    context.yield_once(&mut Greeter).await; // process one message

    let _ = address
        .send(Hello("world"))
        .split_receiver()
        .now_or_never()
        .expect("be able to queue another message because the mailbox is empty again");

    context.yield_once(&mut Greeter).await; // process one message

    // Priority send

    let (address, mut context) = Context::new(Some(1));

    let _ = address
        .send(Hello("world"))
        .priority(1)
        .split_receiver()
        .now_or_never()
        .expect("be able to queue 1 priority message because the mailbox is empty");
    let handler2 = address
        .send(Hello("world"))
        .priority(1)
        .split_receiver()
        .now_or_never();
    assert!(
        handler2.is_none(),
        "Fail to queue 2nd priority message because mailbox is full"
    );

    context.yield_once(&mut Greeter).await; // process one message

    let _ = address
        .send(BroadcastHello("world"))
        .priority(1)
        .split_receiver()
        .now_or_never()
        .expect("be able to queue another priority message because the mailbox is empty again");

    // Broadcast

    let _ = address
        .broadcast(BroadcastHello("world"))
        .priority(2)
        .now_or_never()
        .expect("be able to queue 1 broadcast because the mailbox is empty");
    let handler2 = address
        .broadcast(BroadcastHello("world"))
        .priority(1)
        .now_or_never();
    assert!(
        handler2.is_none(),
        "Fail to queue 2nd broadcast because mailbox is full"
    );

    context.yield_once(&mut Greeter).await; // process one message

    let _ = address
        .broadcast(BroadcastHello("world"))
        .priority(2)
        .now_or_never()
        .expect("be able to queue another broadcast because the mailbox is empty again");
}

#[test]
fn address_debug() {
    let (addr1, _ctx) = Context::<Greeter>::new(None);

    let addr2 = addr1.clone();
    let weak_addr = addr2.downgrade();

    assert_eq!(
        format!("{:?}", addr1),
        "Address(Sender<basic::Greeter> { \
        shutdown: false, rx_count: 1, tx_count: 2, rc: TxStrong(()) })"
    );

    assert_eq!(format!("{:?}", addr1), format!("{:?}", addr2));

    assert_eq!(
        format!("{:?}", weak_addr),
        "Address(Sender<basic::Greeter> { \
        shutdown: false, rx_count: 1, tx_count: 2, rc: TxWeak(()) })"
    );
}

#[test]
fn scoped_task() {
    // Completes when address is connected
    let (addr, ctx) = Context::<Greeter>::new(None);
    let scoped = xtra::scoped(&addr, futures_util::future::ready(()));
    assert!(scoped.now_or_never().is_some());

    // Does not complete when address starts off from a disconnected weak
    let weak = addr.downgrade();
    drop(addr);
    assert!(ctx.run(Greeter).now_or_never().is_some());
    let scoped = xtra::scoped(&weak, futures_util::future::ready(()));
    assert_eq!(scoped.now_or_never(), Some(None));

    // Does not complete when address starts off from a disconnected strong
    let (addr, ctx) = Context::<ActorStopSelf>::new(None);
    let _ = addr.send(StopSelf).split_receiver().now_or_never().unwrap();
    assert!(ctx.run(ActorStopSelf).now_or_never().is_some());
    let scoped = xtra::scoped(&addr, futures_util::future::ready(()));
    assert_eq!(scoped.now_or_never(), Some(None));

    // Does not complete when address disconnects after ScopedTask creation but before first poll
    let (addr, ctx) = Context::<Greeter>::new(None);
    drop(ctx);
    let scoped = xtra::scoped(&addr, futures_util::future::ready(()));
    assert_eq!(scoped.now_or_never(), Some(None));

    // Does not complete when address disconnects after ScopedTask creation but before first poll
    let (addr, act_ctx) = Context::<Greeter>::new(None);
    let mut scoped = xtra::scoped(&addr, futures_util::future::pending::<()>()).boxed();
    let mut fut_ctx = std::task::Context::from_waker(noop_waker_ref());
    assert!(scoped.poll_unpin(&mut fut_ctx).is_pending());
    drop(act_ctx);
    assert_eq!(scoped.poll_unpin(&mut fut_ctx), Poll::Ready(None));
}

#[test]
fn test_addr_cmp_hash_eq() {
    let addr1 = Greeter.create(None).run().0;
    let addr2 = Greeter.create(None).run().0;

    assert_ne!(addr1, addr2);
    assert_ne!(addr1, addr1.downgrade());
    assert_eq!(addr1, addr1.clone());
    assert_ne!(addr1, addr2);
    assert!(addr1.same_actor(&addr1));
    assert!(addr1.same_actor(&addr1.downgrade()));
    assert!(!addr1.same_actor(&addr2));
    assert!(addr1 > addr1.downgrade());

    let chan1 = &addr1 as &dyn StrongMessageChannel<Hello, Return = String>;
    let chan2 = &addr2 as &dyn StrongMessageChannel<Hello, Return = String>;
    assert!(chan1.eq(chan1.upcast_ref()));
    assert!(chan1.eq(chan1.upcast_ref()));
    assert!(!chan1.eq(chan2.upcast_ref()));
    assert!(chan1.same_actor(chan1.upcast_ref()));
    assert!(chan1.same_actor(chan1.downgrade().upcast_ref()));
    assert!(!chan1.same_actor(chan2.upcast_ref()));
}

#[tokio::test]
async fn stop_in_started_actor_stops_immediately() {
    let (_address, ctx) = Context::new(None);
    let fut = ctx.run(StopInStarted);

    fut.now_or_never().unwrap(); // if it stops immediately, this returns `Some`
}

struct StopInStarted;

#[async_trait]
impl Actor for StopInStarted {
    type Stop = ();

    async fn started(&mut self, ctx: &mut Context<Self>) {
        ctx.stop_self();
    }

    async fn stopped(self) -> Self::Stop {}
}

#[tokio::test]
async fn stop_all_stops_immediately() {
    let (_address, mut ctx) = Context::new(None);

    let fut1 = ctx.attach(InstantShutdownAll {
        stop_all: true,
        number: 1,
    });
    let fut2 = ctx.attach(InstantShutdownAll {
        stop_all: false,
        number: 2,
    });
    let fut3 = ctx.attach(InstantShutdownAll {
        stop_all: false,
        number: 3,
    });

    fut1.now_or_never().expect("Should stop immediately");
    fut2.now_or_never().expect("Should stop immediately");
    fut3.now_or_never().expect("Should stop immediately");
}

struct InstantShutdownAll {
    stop_all: bool,
    number: u8,
}

#[async_trait]
impl Actor for InstantShutdownAll {
    type Stop = ();

    async fn started(&mut self, ctx: &mut Context<Self>) {
        if self.stop_all {
            ctx.stop_all();
        }
    }

    async fn stopped(self) {
        println!("Actor {} stopped", self.number);
    }
}
