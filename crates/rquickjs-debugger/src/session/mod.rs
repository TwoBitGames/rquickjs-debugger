mod breakpoints;
mod debugger;
mod dispatch;
mod pause;
mod runtime;
mod scripts;
mod values;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

use rquickjs::{Context, Ctx, Object, Persistent};
use serde_json::{Value as Json, json};
use tracing::{debug, trace, warn};

use crate::engine::{Runtime, StepMode};
use breakpoints::Breakpoint;
use scripts::Script;
pub use scripts::ScriptLog;
use values::RemoteObjects;

pub type Outbound = Box<dyn Fn(String) + Send>;

pub enum Inbound {
    Connected(Outbound),
    Message(String),
    Disconnected,
}

impl std::fmt::Debug for Inbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Inbound::Connected(_) => f.write_str("Connected"),
            Inbound::Message(text) => f.debug_tuple("Message").field(text).finish(),
            Inbound::Disconnected => f.write_str("Disconnected"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Deadline {
    pub at: Rc<Cell<Instant>>,
    pub budget: Duration,
}

impl Deadline {
    fn extend(&self) {
        self.at.set(Instant::now() + self.budget);
    }
}

#[derive(Clone)]
pub struct Attachment {
    pub context: Context,
    pub scripts: ScriptLog,
    pub deadline: Option<Deadline>,
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    Log,
    Debug,
    Info,
    Warn,
    Error,
}

impl ConsoleLevel {
    fn protocol_name(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Target(Rc<RefCell<Session>>);

impl Target {
    pub fn new(name: &str, inbox: Receiver<Inbound>) -> Target {
        Target(Rc::new(RefCell::new(Session::new(name, inbox))))
    }

    pub fn attach(&self, attachment: Attachment) {
        let opaque = Rc::as_ptr(&self.0).cast_mut().cast::<c_void>();
        self.session().attach(attachment, opaque);
    }

    pub fn detach(&self) {
        self.session().detach();
    }

    pub fn detach_from(&self, context: &Context) {
        self.session().detach_from(context);
    }

    pub fn poll(&self) {
        self.with(Session::poll);
    }

    pub fn console(&self, level: ConsoleLevel, text: &str) {
        self.with(|session| session.console(level, text));
    }

    pub fn exception(&self, text: &str) {
        self.with(|session| session.exception(text));
    }

    fn session(&self) -> std::cell::RefMut<'_, Session> {
        self.0.try_borrow_mut().expect(
            "the debugger session is busy serving a pause; attach and detach from the thread \
             that runs the JavaScript, outside debugger callbacks",
        )
    }

    fn with(&self, f: impl FnOnce(&mut Session)) {
        if let Ok(mut session) = self.0.try_borrow_mut() {
            f(&mut session);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Stay,
    Resume(StepMode),
}

struct Attached {
    runtime: Runtime,
    context: Context,
    scripts: ScriptLog,
    deadline: Option<Deadline>,
    helper: Option<Persistent<Object<'static>>>,
}

pub(crate) struct Session {
    name: String,
    inbox: Receiver<Inbound>,
    client: Option<Outbound>,
    deferred: Vec<Json>,
    runtime_enabled: bool,
    debugger_enabled: bool,
    skip_all_pauses: bool,
    breakpoints_active: bool,
    pause_on_exceptions: bool,
    pause_pending: bool,
    breakpoints: Vec<Breakpoint>,
    scripts: Vec<Script>,
    objects: RemoteObjects,
    next_id: u64,
    context_id: u64,
    paused: bool,
    pause_seq: u64,
    attached: Option<Attached>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("name", &self.name)
            .field("attached", &self.attached.is_some())
            .field("paused", &self.paused)
            .field("breakpoints", &self.breakpoints.len())
            .field("scripts", &self.scripts.len())
            .finish_non_exhaustive()
    }
}

impl Session {
    fn new(name: &str, inbox: Receiver<Inbound>) -> Session {
        Session {
            name: name.to_string(),
            inbox,
            client: None,
            deferred: Vec::new(),
            runtime_enabled: false,
            debugger_enabled: false,
            skip_all_pauses: false,
            breakpoints_active: true,
            pause_on_exceptions: false,
            pause_pending: false,
            breakpoints: Vec::new(),
            scripts: Vec::new(),
            objects: RemoteObjects::default(),
            next_id: 0,
            context_id: 0,
            paused: false,
            pause_seq: 0,
            attached: None,
        }
    }

    fn attach(&mut self, attachment: Attachment, opaque: *mut c_void) {
        self.detach();
        let Attachment {
            context,
            scripts,
            deadline,
        } = attachment;
        let runtime = Runtime::of(&context);
        unsafe { runtime.install_handler(pause::on_pause, opaque) };
        runtime.set_pause_on_exceptions(self.pause_on_exceptions);
        runtime.set_breakpoints_active(self.breakpoints_active);
        if std::mem::take(&mut self.pause_pending) && self.client.is_some() {
            runtime.request_pause();
        }
        self.context_id += 1;
        self.scripts.clear();
        self.attached = Some(Attached {
            runtime,
            context: context.clone(),
            scripts,
            deadline,
            helper: None,
        });
        for breakpoint in &mut self.breakpoints {
            breakpoint.forget_runtime();
        }
        context.with(|ctx| {
            for index in 0..self.breakpoints.len() {
                self.register_breakpoint(&ctx, index);
            }
        });
        if self.runtime_enabled {
            self.event(
                "Runtime.executionContextCreated",
                json!({ "context": self.context_json() }),
            );
        }
    }

    fn detach_from(&mut self, context: &Context) {
        let runtime = Runtime::of(context);
        if self.attached.as_ref().is_some_and(|a| a.runtime == runtime) {
            self.detach();
        }
    }

    fn detach(&mut self) {
        let Some(attached) = self.attached.take() else {
            return;
        };
        attached.runtime.remove_handler();
        attached.runtime.clear_breakpoints();
        self.objects.clear();
        drop(attached);
        self.paused = false;
        if self.runtime_enabled {
            self.event(
                "Runtime.executionContextDestroyed",
                json!({
                    "executionContextId": self.context_id,
                    "executionContextUniqueId": self.unique_context_id(),
                }),
            );
            self.event("Runtime.executionContextsCleared", json!({}));
        }
    }

    fn poll(&mut self) {
        let mut messages = Vec::new();
        while let Ok(message) = self.inbox.try_recv() {
            messages.push(message);
        }
        let new_scripts = self
            .attached
            .as_ref()
            .is_some_and(|a| !a.scripts.is_empty());
        if messages.is_empty() && !new_scripts {
            return;
        }
        self.with_ctx(|session, ctx| {
            for message in messages {
                session.inbound(ctx, message);
            }
            session.flush_scripts(ctx);
        });
    }

    fn console(&mut self, level: ConsoleLevel, text: &str) {
        if !self.runtime_enabled || self.client.is_none() {
            return;
        }
        self.event(
            "Runtime.consoleAPICalled",
            json!({
                "type": level.protocol_name(),
                "args": [{ "type": "string", "value": text }],
                "executionContextId": self.context_id,
                "timestamp": now_ms(),
            }),
        );
    }

    fn exception(&mut self, text: &str) {
        if !self.runtime_enabled || self.client.is_none() {
            return;
        }
        let id = self.fresh_id();
        self.event(
            "Runtime.exceptionThrown",
            json!({
                "timestamp": now_ms(),
                "exceptionDetails": {
                    "exceptionId": id,
                    "text": text,
                    "lineNumber": 0,
                    "columnNumber": 0,
                    "executionContextId": self.context_id,
                },
            }),
        );
    }

    fn inbound(&mut self, ctx: Option<&Ctx<'_>>, message: Inbound) -> Flow {
        match message {
            Inbound::Connected(outbound) => {
                if self.client.is_some() {
                    debug!(
                        "debugger '{}': a new client replaces the old one",
                        self.name
                    );
                }
                self.reset_client(ctx);
                self.client = Some(outbound);
                Flow::Stay
            }
            Inbound::Disconnected => {
                self.reset_client(ctx);
                self.client = None;
                Flow::Resume(StepMode::Continue)
            }
            Inbound::Message(text) => self.handle(ctx, &text),
        }
    }

    fn reset_client(&mut self, ctx: Option<&Ctx<'_>>) {
        self.runtime_enabled = false;
        self.debugger_enabled = false;
        self.skip_all_pauses = false;
        self.pause_pending = false;
        self.clear_breakpoints(ctx);
        self.objects.clear();
        for script in &mut self.scripts {
            script.announced = false;
        }
        self.set_pause_on_exceptions(false);
        self.set_breakpoints_active(true);
    }

    fn send(&self, message: &Json) {
        if let Some(client) = &self.client {
            let text = message.to_string();
            trace!(target: "cdp", "{} -> {}", self.name, &text[..text.len().min(400)]);
            client(text);
        }
    }

    fn event(&self, method: &str, params: Json) {
        self.send(&json!({ "method": method, "params": params }));
    }

    fn defer_event(&mut self, method: &str, params: Json) {
        self.deferred
            .push(json!({ "method": method, "params": params }));
    }

    fn with_ctx<R>(&mut self, f: impl FnOnce(&mut Self, Option<&Ctx<'_>>) -> R) -> R {
        match self.attached.as_ref().map(|a| a.context.clone()) {
            Some(context) => context.with(|ctx| f(self, Some(&ctx))),
            None => f(self, None),
        }
    }

    fn runtime(&self) -> Option<Runtime> {
        self.attached.as_ref().map(|a| a.runtime)
    }

    fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn unique_context_id(&self) -> String {
        format!("{}-{}", self.name, self.context_id)
    }

    fn context_json(&self) -> Json {
        json!({
            "id": self.context_id,
            "origin": "",
            "name": self.name,
            "uniqueId": self.unique_context_id(),
            "auxData": { "isDefault": true },
        })
    }

    fn without_pausing<R>(&self, f: impl FnOnce() -> R) -> R {
        let Some(attached) = &self.attached else {
            return f();
        };
        if let Some(deadline) = &attached.deadline {
            deadline.extend();
        }
        attached.runtime.without_pausing(f)
    }

    fn extend_deadline(&self) {
        if let Some(deadline) = self.attached.as_ref().and_then(|a| a.deadline.as_ref()) {
            deadline.extend();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.attached.is_some() {
            warn!(
                "debugger '{}': dropped while attached; detaching",
                self.name
            );
            self.client = None;
            self.detach();
        }
    }
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}
