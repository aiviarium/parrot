//! The Parrot runtime as a state machine without I/O: calls and driver updates in, effects out.
//!
//! `Core::handle` is the only entry point. Each module owns one part of `State` and handles the
//! methods of its area; `dispatch` routes a method to its module.

pub mod clock;
pub mod effect;
pub mod input;
pub mod state;

pub mod actions;
pub mod bindings;
pub mod items;
pub mod options;
pub mod principals;
pub mod queue;
pub mod requests;
pub mod sessions;
pub mod subscriptions;
pub mod usage;

pub mod budgets;
pub mod bundle;
pub mod ceilings;
pub mod seeds;
pub mod tasks;
pub mod tree;
pub mod wakes;

mod dispatch;

pub use clock::{Clock, Ids};
pub use effect::Effect;
pub use input::{BindingId, ConnId, Input, Peer};
pub use state::State;

/// What a module handler gets: the state, the clock and ids, and the effects to emit.
pub struct Cx<'a> {
    pub state: &'a mut State,
    pub now_ms: u64,
    pub ids: &'a mut dyn Ids,
    pub out: &'a mut Vec<Effect>,
}

impl Cx<'_> {
    pub fn emit(&mut self, effect: Effect) {
        self.out.push(effect);
    }
    /// The next value of the runtime-wide sequence.
    pub fn next_seq(&mut self) -> u64 {
        self.state.seq += 1;
        self.state.seq
    }
    /// An id for a call to a driver or client.
    pub fn next_call(&mut self) -> u64 {
        self.state.next_call += 1;
        self.state.next_call
    }
}

pub struct Core {
    pub state: State,
    clock: Box<dyn Clock>,
    ids: Box<dyn Ids>,
}

impl Core {
    pub fn new(clock: Box<dyn Clock>, ids: Box<dyn Ids>) -> Self {
        Self {
            state: State::default(),
            clock,
            ids,
        }
    }

    /// Feed one input; returns the effects to execute, in order.
    pub fn handle(&mut self, input: Input) -> Vec<Effect> {
        let mut out = Vec::new();
        let now_ms = match &input {
            Input::Tick { now_ms } => *now_ms,
            _ => self.clock.now_ms(),
        };
        let mut cx = Cx {
            state: &mut self.state,
            now_ms,
            ids: self.ids.as_mut(),
            out: &mut out,
        };
        dispatch::input(&mut cx, input);
        out
    }
}
