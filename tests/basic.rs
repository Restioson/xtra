use std::cmp::Ordering as CmpOrdering;
use std::ops::ControlFlow;
use std::task::Poll;
use std::time::Duration;

use futures_util::task::noop_waker_ref;
use futures_util::FutureExt;
use smol_timeout::TimeoutExt;
use xtra::prelude::*;
use xtra::Error;

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
    let (addr, ctx) = Context::new(None);
    tokio::spawn(ctx.run(Accumulator(0)));
    for _ in 0..10 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    assert_eq!(addr.send(Report).await.unwrap().0, 10);
}

struct StopTester;

#[async_trait]
impl Actor for StopTester {
    type Stop = ();

    async fn stopped(self) {}
}

struct StopAll;

struct StopSelf;

#[async_trait]
impl Handler<StopSelf> for StopTester {
    type Return = ();

    async fn handle(&mut self, _: StopSelf, ctx: &mut Context<Self>) {
        ctx.stop_self();
    }
}

#[async_trait]
impl Handler<StopAll> for StopTester {
    type Return = ();

    async fn handle(&mut self, _: StopAll, ctx: &mut Context<Self>) {
        ctx.stop_all();
    }
}

#[tokio::test]
async fn actor_stops_on_last_drop_of_address() {
    let (addr, context) = Context::new(None);
    let handle = tokio::spawn(context.run(StopTester));
    let weak = addr.downgrade();
    let join = weak.join();

    drop(addr);
    handle.await.unwrap();

    assert!(!weak.is_connected());
    assert!(join.now_or_never().is_some());
}

#[tokio::test]
async fn actor_stops_on_stop_self_message() {
    let (addr, context) = Context::new(None);
    let handle = tokio::spawn(context.run(StopTester));
    let weak = addr.downgrade();
    let join = weak.join();

    let _ = addr.send(StopSelf).split_receiver().await;
    handle.await.unwrap();

    assert!(!weak.is_connected());
    assert!(!addr.is_connected());
    assert!(join.now_or_never().is_some());
}

#[tokio::test]
async fn actor_stops_on_stop_all_message() {
    let (addr, context) = Context::new(None);
    let handle = tokio::spawn(context.run(StopTester));
    let weak = addr.downgrade();
    let join = weak.join();

    let _ = addr.send(StopAll).split_receiver().await;
    handle.await.unwrap();

    assert!(!weak.is_connected());
    assert!(!addr.is_connected());
    assert!(join.now_or_never().is_some());
}

#[tokio::test]
async fn actor_stops_on_last_drop_of_address_even_if_not_yet_running() {
    let (addr, context) = Context::new(None);
    let weak = addr.downgrade();
    let join = weak.join();

    drop(addr);
    tokio::spawn(context.run(StopTester)).await.unwrap();

    assert!(!weak.is_connected());
    assert!(join.now_or_never().is_some());
}

#[tokio::test]
async fn actor_stops_on_stop_message_even_if_not_yet_running() {
    let (addr, context) = Context::new(None);
    let weak = addr.downgrade();
    let join = weak.join();

    let _ = addr.send(StopSelf).split_receiver().await;
    tokio::spawn(context.run(StopTester)).await.unwrap();

    assert!(!weak.is_connected());
    assert!(!addr.is_connected());
    assert!(join.now_or_never().is_some());
}

#[tokio::test]
async fn handle_left_messages() {
    let (addr, ctx) = Context::new(None);

    for _ in 0..10 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    drop(addr);

    assert_eq!(ctx.run(Accumulator(0)).await, 10);
}

#[tokio::test]
async fn actor_can_be_restarted() {
    let (addr, ctx) = Context::new(None);

    for _ in 0..5 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    let _ = addr.send(StopSelf).split_receiver().await;

    for _ in 0..5 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    assert_eq!(ctx.run(Accumulator(0)).await, 5);

    let (addr, ctx) = Context::new(None);
    let fut1 = ctx.clone().run(Accumulator(0));
    let fut2 = ctx.run(Accumulator(0));

    for _ in 0..5 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    let _ = addr.send(StopAll).split_receiver().await;

    assert_eq!(fut1.await, 5);
    assert!(addr.is_connected());

    // These should not be handled, as fut2 should get the stop_all in this mailbox and stop the
    // actor
    for _ in 0..5 {
        let _ = addr.send(Inc).split_receiver().await;
    }

    assert_eq!(fut2.await, 0);
    assert!(!addr.is_connected());
}

#[tokio::test]
async fn single_actor_on_address_with_stop_self_returns_disconnected_on_stop() {
    let (address, ctx) = Context::new(None);
    let fut = ctx.run(ActorStopSelf);
    let _ = address.send(StopSelf).split_receiver().await;
    assert!(fut.now_or_never().is_some());
    assert!(address.join().now_or_never().is_some());
    assert!(!address.is_connected());
}

#[tokio::test]
async fn two_actors_on_address_with_stop_self() {
    let (address, ctx) = Context::new(None);
    tokio::spawn(ctx.clone().run(ActorStopSelf));
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
    let (address, ctx) = Context::new(None);
    tokio::spawn(ctx.clone().run(ActorStopSelf));
    tokio::spawn(ctx.clone().run(ActorStopSelf)); // Context not dropped here
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
    let address = xtra::spawn_tokio(LongRunningHandler, None);

    let send_future = address.send(Duration::from_secs(3)).split_receiver();
    let handler_future = send_future
        .now_or_never()
        .expect("Dispatch should be immediate on first poll")
        .expect("Actor is not disconnected");

    handler_future.await.unwrap();
}

#[tokio::test]
async fn receiving_async_on_message_channel_returns_immediately_after_dispatch() {
    let address = xtra::spawn_tokio(LongRunningHandler, None);
    let channel = MessageChannel::new(address);

    let send_future = channel.send(Duration::from_secs(3)).split_receiver();
    let handler_future = send_future
        .now_or_never()
        .expect("Dispatch should be immediate on first poll")
        .expect("Actor is not disconnected");

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
        let (ele, ctx) = Context::new(None);
        let fut = ctx.run(Elephant::default());

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
        let (addr, ctx) = Context::new(None);
        let fut = ctx.run(Elephant::default());

        let channel = MessageChannel::new(addr);

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

    let mut indlovu = Box::pin(ctx.clone().run(Elephant::default()));

    addr.broadcast(Message::Broadcast { priority: 0 })
        .priority(0)
        .await
        .unwrap();

    assert!(
        addr.broadcast(Message::Broadcast { priority: 0 })
            .priority(0)
            .now_or_never()
            .is_none(),
        "New broadcast message should not be accepted when tail = len",
    );

    let mut send = addr
        .broadcast(Message::Broadcast { priority: 0 })
        .priority(0);

    assert!(
        send.poll_unpin(&mut fut_ctx).is_pending(),
        "New broadcast message should not be accepted when tail = len"
    );
    assert!(indlovu.poll_unpin(&mut fut_ctx).is_pending());
    ctx.yield_once(&mut ngwevu).await;

    assert!(
        addr.broadcast(Message::Broadcast { priority: 0 })
            .priority(0)
            .now_or_never()
            .is_none(),
        "New broadcast message should not be accepted when tail = len and one is already waiting",
    );

    assert!(
        send.poll_unpin(&mut fut_ctx).is_ready(),
        "New broadcast message should be accepted now"
    );

    assert!(indlovu.poll_unpin(&mut fut_ctx).is_pending());
    ctx.yield_once(&mut ngwevu).await;

    assert_eq!(
        addr.broadcast(Message::Broadcast { priority: 0 })
            .priority(0)
            .await,
        Ok(()),
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
    assert!(indlovu.poll_unpin(&mut fut_ctx).is_pending());

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
    assert!(indlovu.poll_unpin(&mut fut_ctx).is_pending());

    assert!(
        lesser.poll_unpin(&mut fut_ctx).is_ready(),
        "Low prio should be sent in now"
    );

    assert!(
        addr.broadcast(Message::Broadcast { priority: 10 })
            .now_or_never()
            .is_none(),
        "Channel should not accept when full",
    )
}

#[tokio::test]
async fn broadcast_tail_advances_bound_2() {
    let (addr, mut ctx) = Context::new(Some(2));
    let mut ngwevu = Elephant {
        name: "Ngwevu",
        msgs: vec![],
    };

    tokio::spawn(ctx.clone().run(Elephant::default()));

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

    let _fut = ctx.clone().run(Elephant::default());

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
struct PrintHello(&'static str);

#[async_trait]
impl Handler<PrintHello> for Greeter {
    type Return = ();

    async fn handle(
        &mut self,
        PrintHello(name): PrintHello,
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
        .send(PrintHello("world"))
        .priority(1)
        .split_receiver()
        .now_or_never()
        .expect("be able to queue another priority message because the mailbox is empty again");

    // Broadcast

    let _ = address
        .broadcast(PrintHello("world"))
        .priority(2)
        .now_or_never()
        .expect("be able to queue 1 broadcast because the mailbox is empty");
    let handler2 = address
        .broadcast(PrintHello("world"))
        .priority(1)
        .now_or_never();
    assert!(
        handler2.is_none(),
        "Fail to queue 2nd broadcast because mailbox is full"
    );

    context.yield_once(&mut Greeter).await; // process one message

    let _ = address
        .broadcast(PrintHello("world"))
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
        rx_count: 1, tx_count: 2, rc: TxStrong(()) })"
    );

    assert_eq!(format!("{:?}", addr1), format!("{:?}", addr2));

    assert_eq!(
        format!("{:?}", weak_addr),
        "Address(Sender<basic::Greeter> { \
        rx_count: 1, tx_count: 2, rc: TxWeak(()) })"
    );
}

#[test]
fn message_channel_debug() {
    let (addr1, _ctx) = Context::<Greeter>::new(None);

    let mc = MessageChannel::<Hello, String>::new(addr1);
    let weak_mc = mc.downgrade();

    assert_eq!(
        format!("{:?}", mc),
        "MessageChannel<basic::Hello, alloc::string::String>(\
            Sender<basic::Greeter> { rx_count: 1, tx_count: 1, rc: TxStrong(()) }\
        )"
    );

    assert_eq!(
        format!("{:?}", weak_mc),
        "MessageChannel<basic::Hello, alloc::string::String>(\
            Sender<basic::Greeter> { rx_count: 1, tx_count: 1, rc: TxWeak(()) }\
        )"
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
    let addr1 = Context::<Greeter>::new(None).0;
    let addr2 = Context::<Greeter>::new(None).0;

    assert_ne!(addr1, addr2);
    assert_ne!(addr1, addr1.downgrade());
    assert_eq!(addr1, addr1.clone());
    assert!(addr1.same_actor(&addr1));
    assert!(addr1.same_actor(&addr1.downgrade()));
    assert!(!addr1.same_actor(&addr2));
    assert!(addr1 > addr1.downgrade());

    let chan1 = MessageChannel::<Hello, String>::new(addr1);
    let chan2 = MessageChannel::<Hello, String>::new(addr2);
    assert!(chan1.eq(&chan1));
    assert!(!chan1.eq(&chan2));
    assert!(chan1.same_actor(&chan1));
    assert!(chan1.same_actor(&chan1.downgrade()));
    assert!(!chan1.same_actor(&chan2));
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
    let (_address, ctx) = Context::new(None);

    let fut1 = ctx.clone().run(InstantShutdownAll {
        stop_all: true,
        number: 1,
    });
    let fut2 = ctx.clone().run(InstantShutdownAll {
        stop_all: false,
        number: 2,
    });
    let fut3 = ctx.run(InstantShutdownAll {
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

struct Pending;

#[async_trait]
impl Handler<Pending> for Greeter {
    type Return = ();

    async fn handle(&mut self, _: Pending, _ctx: &mut Context<Self>) {
        futures_util::future::pending().await
    }
}

#[tokio::test]
async fn timeout_returns_interrupted() {
    let (address, mut ctx) = Context::new(None);
    let mut actor = Greeter;

    tokio::spawn(async move {
        actor.started(&mut ctx).await;

        if !ctx.running {
            return actor.stopped().await;
        }

        loop {
            let msg = ctx.next_message().await;

            let ctrl = ctx
                .tick(msg, &mut actor)
                .timeout(Duration::from_secs(1))
                .await;

            if let Some(ControlFlow::Break(_)) = ctrl {
                break;
            }
        }
    });

    address
        .send(Hello("world"))
        .split_receiver()
        .now_or_never()
        .expect("Boundless message should be sent instantly")
        .expect("Actor is not disconnected")
        .timeout(Duration::from_secs(3))
        .await
        .expect("Message should not time out")
        .expect("Counter should not be dropped");

    assert_eq!(
        address.send(Pending).await,
        Err(Error::Interrupted),
        "Timeout should return Interrupted"
    );

    address
        .send(Hello("world"))
        .split_receiver()
        .now_or_never()
        .expect("Boundless message should be sent instantly")
        .expect("Actor is not disconnected")
        .timeout(Duration::from_secs(3))
        .await
        .expect("Message should not time out")
        .expect("Counter should not be dropped");

    let weak = address.downgrade();
    let join = address.join();
    drop(address);

    join.await;
    assert_eq!(
        weak.send(Hello("world")).now_or_never(),
        Some(Err(Error::Disconnected)),
        "Interrupt should not be returned after actor stops"
    );
}

#[test]
fn no_sender_returns_disconnected() {
    let (addr, ctx) = Context::<Greeter>::new(None);
    drop(addr);
    assert!(!ctx.weak_address().is_connected());
}

#[tokio::test]
async fn receive_future_can_dispatch_in_one_poll() {
    let (addr, ctx) = Context::<Greeter>::new(None);

    let _ = addr.send(Hello("world")).split_receiver().await;
    let receive_future = ctx.next_message();

    assert!(receive_future.now_or_never().is_some())
}

#[tokio::test]
async fn receive_future_can_dispatch_in_one_poll_after_it_has_been_polled() {
    let (addr, ctx) = Context::<Greeter>::new(None);
    let mut receive_future = ctx.next_message();

    let poll = receive_future.poll_unpin(&mut std::task::Context::from_waker(
        futures_util::task::noop_waker_ref(),
    ));
    assert!(poll.is_pending());

    let _ = addr.send(Hello("world")).split_receiver().await;

    assert!(receive_future.now_or_never().is_some())
}
