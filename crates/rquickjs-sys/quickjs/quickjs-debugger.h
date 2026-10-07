#ifndef QUICKJS_DEBUGGER_H
#define QUICKJS_DEBUGGER_H

#include <stddef.h>

#include "quickjs.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum JSDebuggerPauseReason {
    JS_DEBUGGER_PAUSE_BREAKPOINT = 1,
    JS_DEBUGGER_PAUSE_STEP = 2,
    JS_DEBUGGER_PAUSE_REQUEST = 3,
    JS_DEBUGGER_PAUSE_EXCEPTION = 4,
} JSDebuggerPauseReason;

typedef enum JSDebuggerStepMode {
    JS_DEBUGGER_STEP_NONE = 0,
    JS_DEBUGGER_STEP_INTO = 1,
    JS_DEBUGGER_STEP_OVER = 2,
    JS_DEBUGGER_STEP_OUT = 3,
} JSDebuggerStepMode;

typedef enum JSDebuggerScope {
    JS_DEBUGGER_SCOPE_LOCAL = 0,
    JS_DEBUGGER_SCOPE_CLOSURE = 1,
} JSDebuggerScope;

typedef void JSDebuggerPauseHandler(JSContext *ctx, int reason,
                                    JSValueConst exception, void *opaque);

JS_EXTERN void JS_DebuggerSetHandler(JSRuntime *rt, JSDebuggerPauseHandler *handler,
                                     void *opaque);

JS_EXTERN int JS_DebuggerSetBreakpoint(JSContext *ctx, const char *filename, int line);
JS_EXTERN int JS_DebuggerRemoveBreakpoint(JSContext *ctx, const char *filename, int line);
JS_EXTERN void JS_DebuggerClearBreakpoints(JSRuntime *rt);
JS_EXTERN void JS_DebuggerSetBreakpointsActive(JSRuntime *rt, int active);

JS_EXTERN void JS_DebuggerRequestPause(JSRuntime *rt);
JS_EXTERN void JS_DebuggerSetPauseOnExceptions(JSRuntime *rt, int on);
JS_EXTERN void JS_DebuggerStep(JSRuntime *rt, int mode);

JS_EXTERN void JS_DebuggerSetSuspended(JSRuntime *rt, int suspended);
JS_EXTERN int JS_DebuggerIsSuspended(JSRuntime *rt);

JS_EXTERN JSValue JS_DebuggerBacktrace(JSContext *ctx);
JS_EXTERN JSValue JS_DebuggerFrameScope(JSContext *ctx, int frame_index, int which);
JS_EXTERN int JS_DebuggerSetVariable(JSContext *ctx, int frame_index, const char *name,
                                     JSValueConst val);
JS_EXTERN JSValue JS_DebuggerEvaluate(JSContext *ctx, int frame_index, const char *expr,
                                      size_t len);
JS_EXTERN JSValue JS_DebuggerScriptPositions(JSContext *ctx, const char *filename);

#ifdef __cplusplus
}
#endif

#endif
