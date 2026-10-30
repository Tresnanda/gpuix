// Exercise the production clock with deterministic GPUI/N-API stand-ins. No UI or JS runtime.
extern crate self as gpui;
extern crate self as napi;
extern crate self as serde_json;
use std::{cell::{Cell, RefCell}, rc::Rc, time::Duration};
pub type Value = f64;
#[macro_export]
macro_rules! json { ({$key:literal:$value:expr}) => { $value }; }
#[derive(Debug)]
pub struct Error;
impl Error { pub fn from_reason(_: &str) -> Self { Self } }
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Copy, PartialEq)]
pub enum Status { Ok, GenericFailure }
pub mod threadsafe_function {
    use super::*;
    pub enum ThreadsafeFunctionCallMode { NonBlocking }
    pub struct ThreadsafeFunction<T>(pub Rc<dyn Fn(T) -> Status>);
    impl<T> ThreadsafeFunction<T> {
        pub fn call(&self, value: Result<T>, _: ThreadsafeFunctionCallMode) -> Status {
            (self.0)(value.unwrap())
        }
    }
}
pub struct App;
type Callback = Box<dyn FnOnce(&mut Window, &mut App)>;
#[derive(Default)]
pub struct Window { callbacks: RefCell<Vec<Callback>> }
impl Window {
    pub fn on_next_frame(&self, callback: impl FnOnce(&mut Window, &mut App) + 'static) {
        self.callbacks.borrow_mut().push(Box::new(callback));
    }
    fn tick(&mut self) {
        let callbacks = self.callbacks.take();
        for callback in callbacks { callback(self, &mut App); }
    }
}
#[allow(unused_imports)]
#[path = "frame_clock.rs"]
mod frame_clock;
fn subscriber(calls: &Rc<Cell<usize>>, status: Status) -> threadsafe_function::ThreadsafeFunction<Value> {
    let calls = calls.clone();
    threadsafe_function::ThreadsafeFunction(Rc::new(move |_| { calls.set(calls.get() + 1); status }))
}
fn expire() { std::thread::sleep(Duration::from_millis(1020)); }
#[test]
fn failed_enqueue_retries_next_frame() {
    let clock = Rc::new(RefCell::new(frame_clock::Clock::default()));
    let calls = Rc::new(Cell::new(0));
    let mut window = Window::default();
    frame_clock::add(clock.clone(), subscriber(&calls, Status::GenericFailure), &window).unwrap();
    window.tick(); window.tick();
    assert_eq!(calls.get(), 2);
}
#[test]
fn lost_acknowledgement_retries_only_after_expiry() {
    let clock = Rc::new(RefCell::new(frame_clock::Clock::default()));
    let calls = Rc::new(Cell::new(0));
    let mut window = Window::default();
    frame_clock::add(clock.clone(), subscriber(&calls, Status::Ok), &window).unwrap();
    window.tick(); window.tick();
    assert_eq!(calls.get(), 1);
    expire(); window.tick();
    assert_eq!(calls.get(), 2);
}
#[test]
fn a_new_subscriber_restarts_a_lost_callback_chain() {
    let clock = Rc::new(RefCell::new(frame_clock::Clock::default()));
    let calls = Rc::new(Cell::new(0));
    let mut window = Window::default();
    frame_clock::add(clock.clone(), subscriber(&calls, Status::Ok), &window).unwrap();
    window.callbacks.borrow_mut().clear();
    expire();
    frame_clock::add(clock.clone(), subscriber(&calls, Status::Ok), &window).unwrap();
    window.tick();
    assert_eq!(calls.get(), 2);
    assert_eq!(window.callbacks.borrow().len(), 1);
}
#[test]
fn an_acknowledgement_restarts_a_lost_callback_chain() {
    let clock = Rc::new(RefCell::new(frame_clock::Clock::default()));
    let calls = Rc::new(Cell::new(0));
    let mut window = Window::default();
    let token = frame_clock::add(clock.clone(), subscriber(&calls, Status::Ok), &window).unwrap();
    window.tick(); window.callbacks.borrow_mut().clear();
    expire(); frame_clock::ack(&clock, token, &window);
    window.tick();
    assert_eq!(calls.get(), 2);
    assert_eq!(window.callbacks.borrow().len(), 1);
    frame_clock::remove(&clock, token);
    window.tick();
    assert!(window.callbacks.borrow().is_empty());
}
#[test]
fn a_late_old_callback_cannot_start_a_second_chain() {
    let clock = Rc::new(RefCell::new(frame_clock::Clock::default()));
    let calls = Rc::new(Cell::new(0));
    let mut window = Window::default();
    frame_clock::add(clock.clone(), subscriber(&calls, Status::Ok), &window).unwrap();
    expire();
    frame_clock::add(clock.clone(), subscriber(&calls, Status::Ok), &window).unwrap();
    assert_eq!(window.callbacks.borrow().len(), 2);
    window.tick();
    assert_eq!(calls.get(), 2);
    assert_eq!(window.callbacks.borrow().len(), 1);
}
