/* Copyright (c) 2025-2026 Resonator LLC. Licensed under MIT. */

/*
 * quickjs_shim.c — Export static inline QuickJS functions as real symbols
 * for Rust FFI. Required because these functions are inline in quickjs.h.
 */

#include "quickjs.h"

void JS_FreeValue_shim(JSContext *ctx, JSValue v)
{
    JS_FreeValue(ctx, v);
}

JSValue JS_NewString_shim(JSContext *ctx, const char *str)
{
    return JS_NewString(ctx, str);
}

const char *JS_ToCString_shim(JSContext *ctx, JSValue val)
{
    return JS_ToCString(ctx, val);
}
