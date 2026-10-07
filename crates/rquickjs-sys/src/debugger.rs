use ::core::ffi::{c_char, c_int, c_void};

use crate::{JSContext, JSRuntime, JSValue, JSValueConst, size_t};

pub const JS_DEBUGGER_PAUSE_BREAKPOINT: c_int = 1;
pub const JS_DEBUGGER_PAUSE_STEP: c_int = 2;
pub const JS_DEBUGGER_PAUSE_REQUEST: c_int = 3;
pub const JS_DEBUGGER_PAUSE_EXCEPTION: c_int = 4;

pub const JS_DEBUGGER_STEP_NONE: c_int = 0;
pub const JS_DEBUGGER_STEP_INTO: c_int = 1;
pub const JS_DEBUGGER_STEP_OVER: c_int = 2;
pub const JS_DEBUGGER_STEP_OUT: c_int = 3;

pub const JS_DEBUGGER_SCOPE_LOCAL: c_int = 0;
pub const JS_DEBUGGER_SCOPE_CLOSURE: c_int = 1;

pub type JSDebuggerPauseHandler = ::core::option::Option<
    unsafe extern "C" fn(
        ctx: *mut JSContext,
        reason: c_int,
        exception: JSValueConst,
        opaque: *mut c_void,
    ),
>;

unsafe extern "C" {
    pub fn JS_DebuggerSetHandler(
        rt: *mut JSRuntime,
        handler: JSDebuggerPauseHandler,
        opaque: *mut c_void,
    );
    pub fn JS_DebuggerSetBreakpoint(
        ctx: *mut JSContext,
        filename: *const c_char,
        line: c_int,
    ) -> c_int;
    pub fn JS_DebuggerRemoveBreakpoint(
        ctx: *mut JSContext,
        filename: *const c_char,
        line: c_int,
    ) -> c_int;
    pub fn JS_DebuggerClearBreakpoints(rt: *mut JSRuntime);
    pub fn JS_DebuggerSetBreakpointsActive(rt: *mut JSRuntime, active: c_int);
    pub fn JS_DebuggerRequestPause(rt: *mut JSRuntime);
    pub fn JS_DebuggerSetPauseOnExceptions(rt: *mut JSRuntime, on: c_int);
    pub fn JS_DebuggerStep(rt: *mut JSRuntime, mode: c_int);
    pub fn JS_DebuggerSetSuspended(rt: *mut JSRuntime, suspended: c_int);
    pub fn JS_DebuggerIsSuspended(rt: *mut JSRuntime) -> c_int;
    pub fn JS_DebuggerBacktrace(ctx: *mut JSContext) -> JSValue;
    pub fn JS_DebuggerFrameScope(ctx: *mut JSContext, frame_index: c_int, which: c_int) -> JSValue;
    pub fn JS_DebuggerSetVariable(
        ctx: *mut JSContext,
        frame_index: c_int,
        name: *const c_char,
        val: JSValueConst,
    ) -> c_int;
    pub fn JS_DebuggerEvaluate(
        ctx: *mut JSContext,
        frame_index: c_int,
        expr: *const c_char,
        len: size_t,
    ) -> JSValue;
    pub fn JS_DebuggerScriptPositions(ctx: *mut JSContext, filename: *const c_char) -> JSValue;
}
