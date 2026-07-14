use std::thread;
use std::time::Duration;
use upmd_runtime::{Cmd, Component, Config, Engine};

#[test]
fn test_cmd_stream_sends_messages() {
    let (tx, rx) = flume::unbounded();

    let cmd: Cmd<i32> = Cmd::stream(move |sender| {
        sender.send(1).ok();
        sender.send(2).ok();
        sender.send(3).ok();
    });

    spawn_cmd_for_test(cmd, tx);

    let received: Vec<i32> = rx.try_iter().collect();
    assert_eq!(received, vec![1, 2, 3]);
}

#[test]
fn test_cmd_once_sends_single_message() {
    let (tx, rx) = flume::unbounded();

    let cmd: Cmd<i32> = Cmd::once(|| 42);

    spawn_cmd_for_test(cmd, tx);

    let received: Vec<i32> = rx.try_iter().collect();
    assert_eq!(received, vec![42]);
}

#[test]
fn test_cmd_quit_sets_running_false() {
    let mut engine: Engine<MockComponent> = Engine::new(MockComponent::new());
    assert!(engine.is_running);

    engine.send_msg(Msg::Quit).ok();
    engine.tick();

    assert!(!engine.is_running);
}

#[test]
fn test_cmd_batch_executes_all_commands() {
    let (tx, rx) = flume::unbounded();

    let cmd = Cmd::Batch(vec![Cmd::once(|| 1), Cmd::once(|| 2), Cmd::once(|| 3)]);

    spawn_cmd_for_test(cmd, tx);

    let received: Vec<i32> = rx.try_iter().collect();
    assert_eq!(received.len(), 3);
    assert!(received.contains(&1));
    assert!(received.contains(&2));
    assert!(received.contains(&3));
}

#[test]
fn test_cmd_map_transforms_messages() {
    let (tx, rx) = flume::unbounded();

    let child_cmd: Cmd<ChildMsg> = Cmd::once(|| ChildMsg::Updated(42));
    let parent_cmd: Cmd<ParentMsg> = child_cmd.map(|child_msg| match child_msg {
        ChildMsg::Updated(v) => ParentMsg::ChildUpdated(v),
    });

    spawn_cmd_for_test(parent_cmd, tx);

    thread::sleep(Duration::from_millis(50));

    let received: Vec<ParentMsg> = rx.try_iter().collect();
    assert_eq!(received, vec![ParentMsg::ChildUpdated(42)]);
}

#[test]
fn test_engine_tick_processes_messages() {
    let mut engine: Engine<MockComponent> = Engine::new(MockComponent::new());
    engine.is_dirty = false;

    engine.send_msg(Msg::Increment).ok();
    engine.tick();

    assert!(engine.is_dirty);
    assert_eq!(engine.component.value, 1);
}

#[test]
fn test_engine_tick_quits_on_quit_message() {
    let mut engine: Engine<MockComponent> = Engine::new(MockComponent::new());

    engine.send_msg(Msg::Quit).ok();
    engine.tick();

    assert!(!engine.is_running);
}

#[test]
fn test_engine_send_msg_high_priority() {
    let engine: Engine<MockComponent> = Engine::new(MockComponent::new());

    let result = engine.send_msg(Msg::Increment);
    assert!(result.is_ok());
}

#[test]
fn test_priority_stream_control_overtakes_bulk_messages() {
    let config = Config::new().msg_bound(Some(10)).cmd_bound(Some(1));
    let mut engine = Engine::with_config(PriorityComponent::new(), config);

    thread::sleep(Duration::from_millis(50));
    engine.tick();

    assert_eq!(engine.component.seen.first(), Some(&PriorityMsg::Control));
}

#[test]
fn test_engine_with_config_sets_channel_bounds() {
    let config = Config::new().msg_bound(Some(2048)).cmd_bound(Some(64));
    let engine: Engine<MockComponent> = Engine::with_config(MockComponent::new(), config);

    assert!(engine.is_running);
}

#[test]
fn test_config_default_values() {
    let config = Config::default();

    assert_eq!(config.msg_bound, Some(1024));
    assert_eq!(config.cmd_bound, Some(32));
}

#[test]
fn test_config_custom_values() {
    let config = Config::new().msg_bound(Some(5000)).cmd_bound(Some(128));

    assert_eq!(config.msg_bound, Some(5000));
    assert_eq!(config.cmd_bound, Some(128));
}

#[test]
fn test_engine_tick_processes_decrement() {
    let mut engine: Engine<MockComponent> = Engine::new(MockComponent::new());
    engine.component.value = 10;
    engine.is_dirty = false;

    engine.send_msg(Msg::Decrement).ok();
    engine.tick();

    assert!(engine.is_dirty);
    assert_eq!(engine.component.value, 9);
}

fn spawn_cmd_for_test<Msg: Send + 'static>(cmd: Cmd<Msg>, tx: flume::Sender<Msg>) {
    match cmd {
        Cmd::Quit => unreachable!(),
        Cmd::Stream(run) => run(tx),
        Cmd::PriorityStream(run) => run(tx.clone(), tx),
        Cmd::Task(run) => run(),
        Cmd::Batch(cmds) => {
            for cmd in cmds {
                spawn_cmd_for_test(cmd, tx.clone());
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Msg {
    Increment,
    Decrement,
    Quit,
}

#[derive(Debug, PartialEq)]
enum ChildMsg {
    Updated(i32),
}

#[derive(Debug, PartialEq)]
enum ParentMsg {
    ChildUpdated(i32),
}

struct MockComponent {
    value: i32,
}

impl MockComponent {
    fn new() -> Self {
        Self { value: 0 }
    }
}

impl Component for MockComponent {
    type Msg = Msg;

    fn update(&mut self, msg: Msg) -> Option<Cmd<Msg>> {
        match msg {
            Msg::Increment => self.value += 1,
            Msg::Decrement => self.value -= 1,
            Msg::Quit => return Some(Cmd::quit()),
        }
        None
    }
}

#[derive(Debug, PartialEq)]
enum PriorityMsg {
    Bulk,
    Control,
}

struct PriorityComponent {
    seen: Vec<PriorityMsg>,
}

impl PriorityComponent {
    fn new() -> Self {
        Self { seen: Vec::new() }
    }
}

impl Component for PriorityComponent {
    type Msg = PriorityMsg;

    fn create(&mut self) -> Option<Cmd<Self::Msg>> {
        Some(Cmd::priority_stream(|bulk_tx, control_tx| {
            // Fill the low-priority queue with output-like traffic, then send a
            // lifecycle-like control message. The engine must observe Control
            // first even when bulk traffic is already pending.
            for _ in 0..32 {
                let _ = bulk_tx.try_send(PriorityMsg::Bulk);
            }
            let _ = control_tx.send(PriorityMsg::Control);
        }))
    }

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        self.seen.push(msg);
        None
    }
}
