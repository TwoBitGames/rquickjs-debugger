#define JS_DEBUGGER_FLAG_LINE 1
#define JS_DEBUGGER_FLAG_BP   2

typedef void js_debugger_position_cb(void *opaque, uint32_t pc, int line,
                                     int col, bool statement_start);

static void js_debugger_walk_positions(JSFunctionBytecode *b,
                                       js_debugger_position_cb *cb,
                                       void *opaque)
{
    const uint8_t *p = b->pc2line_buf, *p_end;
    int pc = 0, line = b->line_num, col = b->col_num, prev_line = -1;

    if (!p)
        return;
    p_end = p + b->pc2line_len;
    while (p < p_end) {
        unsigned int op = *p++;
        int new_line, new_col, ret;
        int32_t v;

        if (op == 0) {
            uint32_t pc_delta;
            ret = get_leb128(&pc_delta, p, p_end);
            if (ret < 0)
                return;
            pc += pc_delta;
            p += ret;
            ret = get_sleb128(&v, p, p_end);
            if (ret < 0)
                return;
            p += ret;
            new_line = line + v;
        } else {
            op -= PC2LINE_OP_FIRST;
            pc += (op / PC2LINE_RANGE);
            new_line = line + (op % PC2LINE_RANGE) + PC2LINE_BASE;
        }
        ret = get_sleb128(&v, p, p_end);
        if (ret < 0)
            return;
        p += ret;
        new_col = col + v;
        if ((uint32_t)pc < (uint32_t)b->byte_code_len)
            cb(opaque, pc, new_line, new_col, new_line != prev_line);
        prev_line = line = new_line;
        col = new_col;
    }
}

static bool js_debugger_has_breakpoint(JSDebugger *d, JSAtom filename, int line)
{
    int i;
    for (i = 0; i < d->breakpoint_count; i++) {
        if (d->breakpoints[i].line == line && d->breakpoints[i].filename == filename)
            return true;
    }
    return false;
}

struct js_debugger_flags_ctx {
    JSDebugger *debugger;
    JSFunctionBytecode *b;
    uint8_t *flags;
};

static void js_debugger_flags_cb(void *opaque, uint32_t pc, int line, int col,
                                 bool statement_start)
{
    struct js_debugger_flags_ctx *c = opaque;
    if (!statement_start)
        return;
    c->flags[pc] |= JS_DEBUGGER_FLAG_LINE;
    if (js_debugger_has_breakpoint(c->debugger, c->b->filename, line))
        c->flags[pc] |= JS_DEBUGGER_FLAG_BP;
}

static uint8_t *js_debugger_flags(JSRuntime *rt, JSFunctionBytecode *b)
{
    JSDebugger *d = &rt->debugger;
    struct js_debugger_flags_ctx c;

    if (b->debugger_flags && b->debugger_generation == d->generation)
        return b->debugger_flags;
    if (!b->debugger_flags) {
        b->debugger_flags = js_mallocz_rt(rt, (size_t)b->byte_code_len + 1);
        if (!b->debugger_flags)
            return NULL;
    } else {
        memset(b->debugger_flags, 0, (size_t)b->byte_code_len + 1);
    }
    c.debugger = d;
    c.b = b;
    c.flags = b->debugger_flags;
    js_debugger_walk_positions(b, js_debugger_flags_cb, &c);
    b->debugger_generation = d->generation;
    return b->debugger_flags;
}

static int js_debugger_stack_depth(JSRuntime *rt)
{
    int n = 0;
    JSStackFrame *sf;
    for (sf = rt->current_stack_frame; sf; sf = sf->prev_frame)
        n++;
    return n;
}

static void js_debugger_pause(JSContext *ctx, JSStackFrame *sf,
                              JSFunctionBytecode *b, uint8_t *cur_pc,
                              int reason, JSValueConst exception)
{
    JSRuntime *rt = ctx->rt;
    JSDebugger *d = &rt->debugger;

    sf->cur_pc = cur_pc;
    d->pause_requested = false;
    d->step_mode = JS_DEBUGGER_STEP_NONE;
    d->paused_frame = sf;
    d->suspended = true;
    d->handler(ctx, reason, exception, d->opaque);
    d->suspended = false;
    d->paused_frame = NULL;

    if (d->step_mode != JS_DEBUGGER_STEP_NONE) {
        int col;
        d->step_frame = sf;
        d->step_depth = js_debugger_stack_depth(rt);
        d->step_line = find_line_num(ctx, b, cur_pc - b->byte_code_buf - 1, &col);
        d->step_file = b->filename;
    }
}

static bool js_debugger_step_done(JSContext *ctx, JSStackFrame *sf,
                                  JSFunctionBytecode *b, uint32_t pc_offset)
{
    JSDebugger *d = &ctx->rt->debugger;
    int depth = js_debugger_stack_depth(ctx->rt);
    int col, line = find_line_num(ctx, b, pc_offset, &col);
    bool same_statement = sf == d->step_frame && depth == d->step_depth &&
                          line == d->step_line && b->filename == d->step_file;

    switch (d->step_mode) {
    case JS_DEBUGGER_STEP_INTO:
        return !same_statement;
    case JS_DEBUGGER_STEP_OVER:
        return depth < d->step_depth || (depth == d->step_depth && !same_statement);
    case JS_DEBUGGER_STEP_OUT:
        return depth < d->step_depth;
    default:
        return false;
    }
}

static no_inline void js_debugger_check(JSContext *ctx, JSStackFrame *sf,
                                        JSFunctionBytecode *b, uint8_t *pc)
{
    JSRuntime *rt = ctx->rt;
    JSDebugger *d = &rt->debugger;
    uint8_t *flags;
    uint32_t offset;
    int reason = 0;

    if (d->suspended)
        return;
    flags = js_debugger_flags(rt, b);
    if (!flags)
        return;
    offset = pc - b->byte_code_buf;
    if (!(flags[offset] & JS_DEBUGGER_FLAG_LINE))
        return;

    if (d->pause_requested)
        reason = JS_DEBUGGER_PAUSE_REQUEST;
    else if ((flags[offset] & JS_DEBUGGER_FLAG_BP) && d->breakpoints_active)
        reason = JS_DEBUGGER_PAUSE_BREAKPOINT;
    else if (d->step_mode != JS_DEBUGGER_STEP_NONE &&
             js_debugger_step_done(ctx, sf, b, offset))
        reason = JS_DEBUGGER_PAUSE_STEP;

    if (reason)
        js_debugger_pause(ctx, sf, b, pc + 1, reason, JS_UNDEFINED);
}

static bool js_debugger_same_object(JSValueConst a, JSValueConst b)
{
    return JS_VALUE_GET_TAG(a) == JS_TAG_OBJECT &&
           JS_VALUE_GET_TAG(b) == JS_TAG_OBJECT &&
           JS_VALUE_GET_PTR(a) == JS_VALUE_GET_PTR(b);
}

static no_inline void js_debugger_exception(JSContext *ctx, JSStackFrame *sf,
                                            JSFunctionBytecode *b, uint8_t *pc)
{
    JSRuntime *rt = ctx->rt;
    JSDebugger *d = &rt->debugger;
    JSValue exception = rt->current_exception;

    if (d->suspended || JS_IsUncatchableError(exception) ||
        JS_IsUninitialized(exception))
        return;
    if (js_debugger_same_object(exception, d->last_exception))
        return;
    JS_FreeValueRT(rt, d->last_exception);
    d->last_exception = js_dup(exception);

    exception = JS_GetException(ctx);
    js_debugger_pause(ctx, sf, b, pc, JS_DEBUGGER_PAUSE_EXCEPTION, exception);
    JS_Throw(ctx, exception);
}

static void js_debugger_clear_breakpoints(JSRuntime *rt)
{
    JSDebugger *d = &rt->debugger;
    int i;
    for (i = 0; i < d->breakpoint_count; i++)
        JS_FreeAtomRT(rt, d->breakpoints[i].filename);
    d->breakpoint_count = 0;
    d->generation++;
}

static void js_debugger_free(JSRuntime *rt)
{
    JSDebugger *d = &rt->debugger;
    js_debugger_clear_breakpoints(rt);
    js_free_rt(rt, d->breakpoints);
    d->breakpoints = NULL;
    d->breakpoint_capacity = 0;
    JS_FreeValueRT(rt, d->last_exception);
    d->last_exception = JS_UNINITIALIZED;
}

void JS_DebuggerSetHandler(JSRuntime *rt, JSDebuggerPauseHandler *handler,
                           void *opaque)
{
    JSDebugger *d = &rt->debugger;
    d->handler = handler;
    d->opaque = opaque;
    d->breakpoints_active = true;
    d->step_mode = JS_DEBUGGER_STEP_NONE;
    d->pause_requested = false;
    if (!handler) {
        JS_FreeValueRT(rt, d->last_exception);
        d->last_exception = JS_UNINITIALIZED;
    }
}

static int js_debugger_find_breakpoint(JSDebugger *d, JSAtom filename, int line)
{
    int i;
    for (i = 0; i < d->breakpoint_count; i++) {
        if (d->breakpoints[i].filename == filename && d->breakpoints[i].line == line)
            return i;
    }
    return -1;
}

int JS_DebuggerSetBreakpoint(JSContext *ctx, const char *filename, int line)
{
    JSRuntime *rt = ctx->rt;
    JSDebugger *d = &rt->debugger;
    JSAtom atom;

    atom = JS_NewAtom(ctx, filename);
    if (atom == JS_ATOM_NULL)
        return -1;
    if (js_debugger_find_breakpoint(d, atom, line) >= 0) {
        JS_FreeAtom(ctx, atom);
        return 0;
    }
    if (d->breakpoint_count >= d->breakpoint_capacity) {
        int capacity = d->breakpoint_capacity ? d->breakpoint_capacity * 2 : 8;
        JSDebuggerBreakpoint *breakpoints =
            js_realloc_rt(rt, d->breakpoints, sizeof(*breakpoints) * capacity);
        if (!breakpoints) {
            JS_FreeAtom(ctx, atom);
            return -1;
        }
        d->breakpoints = breakpoints;
        d->breakpoint_capacity = capacity;
    }
    d->breakpoints[d->breakpoint_count].filename = atom;
    d->breakpoints[d->breakpoint_count].line = line;
    d->breakpoint_count++;
    d->generation++;
    return 0;
}

int JS_DebuggerRemoveBreakpoint(JSContext *ctx, const char *filename, int line)
{
    JSDebugger *d = &ctx->rt->debugger;
    JSAtom atom;
    int i;

    atom = JS_NewAtom(ctx, filename);
    if (atom == JS_ATOM_NULL)
        return -1;
    i = js_debugger_find_breakpoint(d, atom, line);
    JS_FreeAtom(ctx, atom);
    if (i < 0)
        return 0;
    JS_FreeAtom(ctx, d->breakpoints[i].filename);
    d->breakpoints[i] = d->breakpoints[--d->breakpoint_count];
    d->generation++;
    return 1;
}

void JS_DebuggerClearBreakpoints(JSRuntime *rt)
{
    js_debugger_clear_breakpoints(rt);
}

void JS_DebuggerSetBreakpointsActive(JSRuntime *rt, int active)
{
    rt->debugger.breakpoints_active = (active != 0);
}

void JS_DebuggerRequestPause(JSRuntime *rt)
{
    rt->debugger.pause_requested = true;
}

void JS_DebuggerSetPauseOnExceptions(JSRuntime *rt, int on)
{
    rt->debugger.pause_on_exceptions = (on != 0);
}

void JS_DebuggerStep(JSRuntime *rt, int mode)
{
    rt->debugger.step_mode = mode;
}

void JS_DebuggerSetSuspended(JSRuntime *rt, int suspended)
{
    rt->debugger.suspended = (suspended != 0);
}

int JS_DebuggerIsSuspended(JSRuntime *rt)
{
    return rt->debugger.suspended;
}

static JSFunctionBytecode *js_debugger_frame_bytecode(JSStackFrame *sf)
{
    JSObject *p;
    if (JS_VALUE_GET_TAG(sf->cur_func) != JS_TAG_OBJECT)
        return NULL;
    p = JS_VALUE_GET_OBJ(sf->cur_func);
    if (!js_class_has_bytecode(p->class_id))
        return NULL;
    return p->u.func.function_bytecode;
}

static JSStackFrame *js_debugger_top_frame(JSRuntime *rt)
{
    return rt->debugger.paused_frame ? rt->debugger.paused_frame
                                     : rt->current_stack_frame;
}

static JSStackFrame *js_debugger_frame(JSRuntime *rt, int index,
                                       JSFunctionBytecode **pb)
{
    JSStackFrame *sf;
    int n = 0;
    for (sf = js_debugger_top_frame(rt); sf; sf = sf->prev_frame) {
        JSFunctionBytecode *b = js_debugger_frame_bytecode(sf);
        if (!b)
            continue;
        if (n++ == index) {
            *pb = b;
            return sf;
        }
    }
    return NULL;
}

static int js_debugger_frame_line(JSContext *ctx, JSStackFrame *sf,
                                  JSFunctionBytecode *b, int *pcol)
{
    if (sf->cur_pc && sf->cur_pc > b->byte_code_buf)
        return find_line_num(ctx, b, sf->cur_pc - b->byte_code_buf - 1, pcol);
    *pcol = b->col_num;
    return b->line_num;
}

static int js_debugger_define_atom_string(JSContext *ctx, JSValueConst obj,
                                          const char *name, JSAtom atom)
{
    JSValue s = atom == JS_ATOM_NULL ? js_empty_string(ctx->rt)
                                     : JS_AtomToString(ctx, atom);
    if (JS_IsException(s))
        return -1;
    return JS_DefinePropertyValueStr(ctx, obj, name, s, JS_PROP_C_W_E);
}

static int js_debugger_define_int(JSContext *ctx, JSValueConst obj,
                                  const char *name, int value)
{
    return JS_DefinePropertyValueStr(ctx, obj, name, js_int32(value), JS_PROP_C_W_E);
}

JSValue JS_DebuggerBacktrace(JSContext *ctx)
{
    JSRuntime *rt = ctx->rt;
    JSValue frames = JS_NewArray(ctx);
    JSStackFrame *sf;
    uint32_t n = 0;

    if (JS_IsException(frames))
        return frames;
    for (sf = js_debugger_top_frame(rt); sf; sf = sf->prev_frame) {
        JSFunctionBytecode *b = js_debugger_frame_bytecode(sf);
        JSValue frame;
        int line, col;

        if (!b)
            continue;
        line = js_debugger_frame_line(ctx, sf, b, &col);
        frame = JS_NewObject(ctx);
        if (JS_IsException(frame))
            goto fail;
        if (js_debugger_define_atom_string(ctx, frame, "functionName", b->func_name) < 0 ||
            js_debugger_define_atom_string(ctx, frame, "fileName", b->filename) < 0 ||
            js_debugger_define_int(ctx, frame, "line", line) < 0 ||
            js_debugger_define_int(ctx, frame, "column", col) < 0 ||
            JS_DefinePropertyValueUint32(ctx, frames, n++, frame, JS_PROP_C_W_E) < 0)
            goto fail;
    }
    return frames;
fail:
    JS_FreeValue(ctx, frames);
    return JS_EXCEPTION;
}

static bool js_debugger_hidden_name(JSContext *ctx, JSAtom name)
{
    const char *s;
    bool hidden;

    if (name == JS_ATOM_NULL || name == JS_ATOM__ret_ || name == JS_ATOM_home_object)
        return true;
    s = JS_AtomToCString(ctx, name);
    if (!s)
        return true;
    hidden = s[0] == '<' || s[0] == '\0';
    JS_FreeCString(ctx, s);
    return hidden;
}

static int js_debugger_define_variable(JSContext *ctx, JSValueConst scope,
                                       JSAtom name, JSValueConst value)
{
    if (JS_IsUninitialized(value) || js_debugger_hidden_name(ctx, name))
        return 0;
    return JS_DefinePropertyValue(ctx, scope, name, js_dup(value), JS_PROP_C_W_E);
}

static int js_debugger_local_scope(JSContext *ctx, JSValueConst scope,
                                   JSStackFrame *sf, JSFunctionBytecode *b)
{
    int i;
    if (!b->vardefs)
        return 0;
    for (i = 0; i < b->arg_count; i++) {
        JSValueConst v = i < sf->arg_count ? sf->arg_buf[i] : JS_UNDEFINED;
        if (js_debugger_define_variable(ctx, scope, b->vardefs[i].var_name, v) < 0)
            return -1;
    }
    for (i = 0; i < b->var_count; i++) {
        JSAtom name = b->vardefs[b->arg_count + i].var_name;
        if (js_debugger_define_variable(ctx, scope, name, sf->var_buf[i]) < 0)
            return -1;
    }
    return 0;
}

static int js_debugger_closure_scope(JSContext *ctx, JSValueConst scope,
                                     JSStackFrame *sf, JSFunctionBytecode *b)
{
    JSObject *p = JS_VALUE_GET_OBJ(sf->cur_func);
    JSVarRef **var_refs = p->u.func.var_refs;
    int i;
    if (!var_refs)
        return 0;
    for (i = 0; i < b->closure_var_count; i++) {
        JSVarRef *var_ref = var_refs[i];
        if (!var_ref)
            continue;
        if (js_debugger_define_variable(ctx, scope, b->closure_var[i].var_name,
                                        *var_ref->pvalue) < 0)
            return -1;
    }
    return 0;
}

JSValue JS_DebuggerFrameScope(JSContext *ctx, int frame_index, int which)
{
    JSFunctionBytecode *b;
    JSStackFrame *sf = js_debugger_frame(ctx->rt, frame_index, &b);
    JSValue scope;
    int ret;

    if (!sf)
        return JS_ThrowInternalError(ctx, "debugger: no such frame");
    scope = JS_NewObject(ctx);
    if (JS_IsException(scope))
        return scope;
    if (which == JS_DEBUGGER_SCOPE_LOCAL)
        ret = js_debugger_local_scope(ctx, scope, sf, b);
    else
        ret = js_debugger_closure_scope(ctx, scope, sf, b);
    if (ret < 0) {
        JS_FreeValue(ctx, scope);
        return JS_EXCEPTION;
    }
    return scope;
}

static JSValue *js_debugger_variable_slot(JSStackFrame *sf,
                                          JSFunctionBytecode *b, JSAtom name)
{
    JSValue *slot = NULL;
    JSObject *p;
    JSVarRef **var_refs;
    int i;

    if (b->vardefs) {
        for (i = 0; i < b->var_count; i++)
            if (b->vardefs[b->arg_count + i].var_name == name)
                slot = &sf->var_buf[i];
        if (slot)
            return slot;
        for (i = 0; i < b->arg_count && i < sf->arg_count; i++)
            if (b->vardefs[i].var_name == name)
                return &sf->arg_buf[i];
    }
    p = JS_VALUE_GET_OBJ(sf->cur_func);
    var_refs = p->u.func.var_refs;
    if (var_refs) {
        for (i = 0; i < b->closure_var_count; i++)
            if (b->closure_var[i].var_name == name && var_refs[i])
                slot = var_refs[i]->pvalue;
    }
    return slot;
}

int JS_DebuggerSetVariable(JSContext *ctx, int frame_index, const char *name,
                           JSValueConst val)
{
    JSFunctionBytecode *b;
    JSStackFrame *sf = js_debugger_frame(ctx->rt, frame_index, &b);
    JSAtom atom;
    JSValue *slot;

    if (!sf)
        return -1;
    atom = JS_NewAtom(ctx, name);
    if (atom == JS_ATOM_NULL)
        return -1;
    slot = js_debugger_variable_slot(sf, b, atom);
    JS_FreeAtom(ctx, atom);
    if (!slot)
        return 0;
    JS_FreeValue(ctx, *slot);
    *slot = js_dup(val);
    return 1;
}

static int js_debugger_guess_scope(JSStackFrame *sf, JSFunctionBytecode *b)
{
    int i;
    if (!b->vardefs)
        return -1;
    for (i = b->var_count - 1; i >= 0; i--) {
        JSVarDef *vd = &b->vardefs[b->arg_count + i];
        if (vd->scope_level > 0 && !JS_IsUninitialized(sf->var_buf[i]))
            return i;
    }
    return -1;
}

JSValue JS_DebuggerEvaluate(JSContext *ctx, int frame_index, const char *expr,
                            size_t len)
{
    JSRuntime *rt = ctx->rt;
    JSDebugger *d = &rt->debugger;
    JSFunctionBytecode *b;
    JSStackFrame *sf = js_debugger_frame(rt, frame_index, &b);
    JSStackFrame *saved_frame;
    bool saved_suspended;
    JSValue ret;

    if (!sf)
        return JS_ThrowInternalError(ctx, "debugger: no such frame");
    saved_frame = rt->current_stack_frame;
    saved_suspended = d->suspended;
    rt->current_stack_frame = sf;
    d->suspended = true;
    ret = JS_EvalInternal(ctx, JS_UNDEFINED, expr, len, "<debugger>", 1,
                          JS_EVAL_TYPE_DIRECT, js_debugger_guess_scope(sf, b));
    d->suspended = saved_suspended;
    rt->current_stack_frame = saved_frame;
    return ret;
}

struct js_debugger_positions_ctx {
    JSContext *ctx;
    JSValue positions;
    uint32_t n;
    bool failed;
};

static void js_debugger_positions_cb(void *opaque, uint32_t pc, int line,
                                     int col, bool statement_start)
{
    struct js_debugger_positions_ctx *c = opaque;
    if (!statement_start || c->failed)
        return;
    if (JS_DefinePropertyValueUint32(c->ctx, c->positions, c->n++, js_int32(line), JS_PROP_C_W_E) < 0 ||
        JS_DefinePropertyValueUint32(c->ctx, c->positions, c->n++, js_int32(col), JS_PROP_C_W_E) < 0)
        c->failed = true;
}

JSValue JS_DebuggerScriptPositions(JSContext *ctx, const char *filename)
{
    JSRuntime *rt = ctx->rt;
    struct js_debugger_positions_ctx c;
    struct list_head *el;
    JSAtom atom;

    atom = JS_NewAtom(ctx, filename);
    if (atom == JS_ATOM_NULL)
        return JS_EXCEPTION;
    c.ctx = ctx;
    c.positions = JS_NewArray(ctx);
    c.n = 0;
    c.failed = false;
    if (JS_IsException(c.positions)) {
        JS_FreeAtom(ctx, atom);
        return JS_EXCEPTION;
    }
    list_for_each(el, &rt->gc_obj_list) {
        JSGCObjectHeader *gp = list_entry(el, JSGCObjectHeader, link);
        JSFunctionBytecode *b;
        if (JS_GC_TYPE(gp) != JS_GC_OBJ_TYPE_FUNCTION_BYTECODE)
            continue;
        b = (JSFunctionBytecode *)gp;
        if (b->filename != atom)
            continue;
        js_debugger_walk_positions(b, js_debugger_positions_cb, &c);
        if (c.failed)
            break;
    }
    JS_FreeAtom(ctx, atom);
    if (c.failed) {
        JS_FreeValue(ctx, c.positions);
        return JS_EXCEPTION;
    }
    return c.positions;
}
