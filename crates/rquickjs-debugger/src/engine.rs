use std::ffi::{CString, c_int, c_void};
use std::ptr::NonNull;

use rquickjs::{Context, Ctx, Object, Value, qjs};

use crate::protocol::Position;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PauseReason {
    Breakpoint,
    Step,
    Request,
    Exception,
}

impl PauseReason {
    pub fn from_raw(raw: c_int) -> Option<Self> {
        match raw {
            qjs::JS_DEBUGGER_PAUSE_BREAKPOINT => Some(Self::Breakpoint),
            qjs::JS_DEBUGGER_PAUSE_STEP => Some(Self::Step),
            qjs::JS_DEBUGGER_PAUSE_REQUEST => Some(Self::Request),
            qjs::JS_DEBUGGER_PAUSE_EXCEPTION => Some(Self::Exception),
            _ => None,
        }
    }

    pub fn protocol_name(self) -> &'static str {
        match self {
            Self::Breakpoint => "breakpoint",
            Self::Step => "step",
            Self::Exception => "exception",
            Self::Request => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum StepMode {
    #[default]
    Continue,
    Into,
    Over,
    Out,
}

impl StepMode {
    fn raw(self) -> c_int {
        match self {
            Self::Continue => qjs::JS_DEBUGGER_STEP_NONE,
            Self::Into => qjs::JS_DEBUGGER_STEP_INTO,
            Self::Over => qjs::JS_DEBUGGER_STEP_OVER,
            Self::Out => qjs::JS_DEBUGGER_STEP_OUT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    Local,
    Closure,
}

impl ScopeKind {
    fn raw(self) -> c_int {
        match self {
            Self::Local => qjs::JS_DEBUGGER_SCOPE_LOCAL,
            Self::Closure => qjs::JS_DEBUGGER_SCOPE_CLOSURE,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub function: String,
    pub file: String,
    pub position: Position,
}

pub(crate) type PauseHandler = unsafe extern "C" fn(
    ctx: *mut qjs::JSContext,
    reason: c_int,
    exception: qjs::JSValueConst,
    opaque: *mut c_void,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Runtime(NonNull<qjs::JSRuntime>);

impl Runtime {
    pub fn of(context: &Context) -> Runtime {
        let raw = context.with(|ctx| unsafe { qjs::JS_GetRuntime(ctx.as_raw().as_ptr()) });
        Runtime(NonNull::new(raw).expect("a live context has a runtime"))
    }

    fn raw(self) -> *mut qjs::JSRuntime {
        self.0.as_ptr()
    }

    pub unsafe fn install_handler(self, handler: PauseHandler, opaque: *mut c_void) {
        unsafe { qjs::JS_DebuggerSetHandler(self.raw(), Some(handler), opaque) }
    }

    pub fn remove_handler(self) {
        unsafe { qjs::JS_DebuggerSetHandler(self.raw(), None, std::ptr::null_mut()) }
    }

    pub fn set_breakpoints_active(self, active: bool) {
        unsafe { qjs::JS_DebuggerSetBreakpointsActive(self.raw(), c_int::from(active)) }
    }

    pub fn set_pause_on_exceptions(self, on: bool) {
        unsafe { qjs::JS_DebuggerSetPauseOnExceptions(self.raw(), c_int::from(on)) }
    }

    pub fn request_pause(self) {
        unsafe { qjs::JS_DebuggerRequestPause(self.raw()) }
    }

    pub fn step(self, mode: StepMode) {
        unsafe { qjs::JS_DebuggerStep(self.raw(), mode.raw()) }
    }

    pub fn clear_breakpoints(self) {
        unsafe { qjs::JS_DebuggerClearBreakpoints(self.raw()) }
    }

    pub fn without_pausing<R>(self, f: impl FnOnce() -> R) -> R {
        unsafe {
            let was = qjs::JS_DebuggerIsSuspended(self.raw());
            qjs::JS_DebuggerSetSuspended(self.raw(), 1);
            let result = f();
            qjs::JS_DebuggerSetSuspended(self.raw(), was);
            result
        }
    }
}

pub(crate) fn set_breakpoint(ctx: &Ctx<'_>, file: &str, line: u32) -> bool {
    let Ok(file) = CString::new(file) else {
        return false;
    };
    unsafe {
        qjs::JS_DebuggerSetBreakpoint(ctx.as_raw().as_ptr(), file.as_ptr(), line as c_int) == 0
    }
}

pub(crate) fn remove_breakpoint(ctx: &Ctx<'_>, file: &str, line: u32) {
    let Ok(file) = CString::new(file) else {
        return;
    };
    unsafe {
        qjs::JS_DebuggerRemoveBreakpoint(ctx.as_raw().as_ptr(), file.as_ptr(), line as c_int);
    }
}

pub(crate) fn backtrace(ctx: &Ctx<'_>) -> Vec<Frame> {
    let raw = unsafe { qjs::JS_DebuggerBacktrace(ctx.as_raw().as_ptr()) };
    let Some(frames) = adopt(ctx, raw).ok().and_then(Value::into_array) else {
        return Vec::new();
    };
    frames
        .iter::<Object<'_>>()
        .filter_map(Result::ok)
        .map(|frame| Frame {
            function: frame.get("functionName").unwrap_or_default(),
            file: frame.get("fileName").unwrap_or_default(),
            position: Position::new(
                frame.get::<_, i32>("line").unwrap_or(1).max(1) as u32,
                frame.get::<_, i32>("column").unwrap_or(1).max(1) as u32,
            ),
        })
        .collect()
}

pub(crate) fn frame_scope<'js>(ctx: &Ctx<'js>, frame: u32, kind: ScopeKind) -> Option<Object<'js>> {
    let raw =
        unsafe { qjs::JS_DebuggerFrameScope(ctx.as_raw().as_ptr(), frame as c_int, kind.raw()) };
    adopt(ctx, raw).ok().and_then(Value::into_object)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetVariable {
    Set,
    NoSuchVariable,
    Failed,
}

pub(crate) fn set_variable(
    ctx: &Ctx<'_>,
    frame: u32,
    name: &str,
    value: &Value<'_>,
) -> SetVariable {
    let Ok(name) = CString::new(name) else {
        return SetVariable::NoSuchVariable;
    };
    let result = unsafe {
        qjs::JS_DebuggerSetVariable(
            ctx.as_raw().as_ptr(),
            frame as c_int,
            name.as_ptr(),
            value.as_raw(),
        )
    };
    match result {
        1 => SetVariable::Set,
        0 => SetVariable::NoSuchVariable,
        _ => SetVariable::Failed,
    }
}

pub(crate) fn evaluate_in_frame<'js>(
    ctx: &Ctx<'js>,
    frame: u32,
    expression: &str,
) -> rquickjs::Result<Value<'js>> {
    let code = CString::new(expression)?;
    let raw = unsafe {
        qjs::JS_DebuggerEvaluate(
            ctx.as_raw().as_ptr(),
            frame as c_int,
            code.as_ptr(),
            code.as_bytes().len() as _,
        )
    };
    adopt(ctx, raw)
}

pub(crate) fn script_positions(ctx: &Ctx<'_>, file: &str) -> Vec<Position> {
    let Ok(file) = CString::new(file) else {
        return Vec::new();
    };
    let raw = unsafe { qjs::JS_DebuggerScriptPositions(ctx.as_raw().as_ptr(), file.as_ptr()) };
    let Some(flat) = adopt(ctx, raw).ok().and_then(Value::into_array) else {
        return Vec::new();
    };
    let numbers: Vec<i32> = flat.iter::<i32>().filter_map(Result::ok).collect();
    let mut positions: Vec<Position> = numbers
        .chunks_exact(2)
        .filter(|pair| pair[0] > 0)
        .map(|pair| Position::new(pair[0] as u32, pair[1].max(1) as u32))
        .collect();
    positions.sort_unstable();
    positions.dedup();
    positions
}

fn adopt<'js>(ctx: &Ctx<'js>, raw: qjs::JSValue) -> rquickjs::Result<Value<'js>> {
    if unsafe { qjs::JS_IsException(raw) } {
        return Err(rquickjs::Error::Exception);
    }
    Ok(unsafe { Value::from_raw(ctx.clone(), raw) })
}
