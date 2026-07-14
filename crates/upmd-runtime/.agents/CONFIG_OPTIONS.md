# Config Options Comparison

## Option A: Builder Pattern

```rust
use cha::core::Engine;

let engine = Engine::builder(component)
    .msg_bound(2048)
    .cmd_bound(64)
    .build();
```

**Implementation:**
```rust
pub struct EngineBuilder<C: Component> {
    component: C,
    msg_bound: usize,
    cmd_bound: usize,
}

impl<C: Component> EngineBuilder<C> {
    pub fn new(component: C) -> Self {
        Self {
            component,
            msg_bound: 1024,
            cmd_bound: 32,
        }
    }

    pub fn msg_bound(mut self, bound: usize) -> Self {
        self.msg_bound = bound;
        self
    }

    pub fn cmd_bound(mut self, bound: usize) -> Self {
        self.cmd_bound = bound;
        self
    }

    pub fn build(self) -> Engine<C> {
        Engine::from_config(self.component, Config {
            msg_bound: self.msg_bound,
            cmd_bound: self.cmd_bound,
        })
    }
}
```

---

## Option B: Config Struct with Default

```rust
use cha::core::{Engine, Config};

let engine = Engine::with_config(
    component,
    Config {
        msg_bound: 2048,
        cmd_bound: 64,
        ..Config::default()
    }
);

// Or with builder-style methods on Config
let engine = Engine::with_config(
    component,
    Config::default().msg_bound(2048).cmd_bound(64)
);
```

**Implementation:**
```rust
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub msg_bound: usize,
    pub cmd_bound: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            msg_bound: 1024,
            cmd_bound: 32,
        }
    }
}

impl Config {
    pub fn msg_bound(mut self, bound: usize) -> Self {
        self.msg_bound = bound;
        self
    }

    pub fn cmd_bound(mut self, bound: usize) -> Self {
        self.cmd_bound = bound;
        self
    }
}
```

---

## Option C: Associated Constants

```rust
// Compile-time only - bounds are set at compile time
let engine = Engine::<1024, 64>::new(component);
```

**Implementation:**
```rust
pub struct Engine<C: Component, const MSG_BOUND: usize = 1024, const CMD_BOUND: usize = 32> {
    // ...
    msg_tx: Sender<C::Msg>,
    msg_rx: Receiver<C::Msg>,
}

impl<C: Component, const MSG_BOUND: usize, const CMD_BOUND: usize> Engine<C, MSG_BOUND, CMD_BOUND> {
    pub fn new(mut component: C) -> Self {
        let (msg_tx, msg_rx) = bounded(MSG_BOUND);
        let (cmd_tx, cmd_rx) = bounded(CMD_BOUND);
        // ...
    }
}
```

---

## Option D: Type Parameters (PhantomData)

```rust
struct MsgBound<const N: usize>;
struct CmdBound<const N: usize>;

let engine = Engine::new::<MsgBound<1024>, CmdBound<64>>(component);
```

---

## Recommendation

**Option A (Builder)** is most idiomatic for Rust - flexible, discoverable, easy to extend.
**Option B (Config struct)** is simpler and good if you want to pass config around.

Avoid C and D - const generics for runtime config is overkill.
