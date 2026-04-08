///! QuickJS VM via C FFI. Each ScriptNode gets its own JSRuntime + JSContext.

use anyhow::{anyhow, bail, Result};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

/// Type for store query requests: (SPARQL, response channel)
pub type QueryRequest = (String, Sender<Vec<Vec<(String, String)>>>);

// ---------------------------------------------------------------------------
// QuickJS FFI types — JSValue is a 16-byte struct (union + tag)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Copy, Clone)]
pub union JSValueUnion {
    pub int32: i32,
    pub float64: f64,
    pub ptr: *mut c_void,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct JSValue {
    pub u: JSValueUnion,
    pub tag: i64,
}

impl JSValue {
    fn is_exception(&self) -> bool {
        self.tag == JS_TAG_EXCEPTION
    }
}

type JSRuntime = c_void;
type JSContext = c_void;

type JSCFunction = unsafe extern "C" fn(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: c_int,
    argv: *mut JSValue,
) -> JSValue;

const JS_EVAL_TYPE_GLOBAL: c_int = 0;
const JS_TAG_INT: i64 = 0;
const JS_TAG_OBJECT: i64 = -1;
const JS_TAG_STRING: i64 = -7;
const JS_TAG_UNDEFINED: i64 = 3;
const JS_TAG_EXCEPTION: i64 = 6;

fn js_undefined() -> JSValue {
    JSValue {
        u: JSValueUnion { int32: 0 },
        tag: JS_TAG_UNDEFINED,
    }
}

fn js_mkval(tag: i64, val: i32) -> JSValue {
    JSValue {
        u: JSValueUnion { int32: val },
        tag,
    }
}

// ---------------------------------------------------------------------------
// FFI declarations
// ---------------------------------------------------------------------------

extern "C" {
    fn JS_NewRuntime() -> *mut JSRuntime;
    fn JS_FreeRuntime(rt: *mut JSRuntime);
    fn JS_SetMemoryLimit(rt: *mut JSRuntime, limit: usize);
    fn JS_NewContext(rt: *mut JSRuntime) -> *mut JSContext;
    fn JS_FreeContext(ctx: *mut JSContext);
    fn JS_Eval(
        ctx: *mut JSContext,
        input: *const c_char,
        input_len: usize,
        filename: *const c_char,
        eval_flags: c_int,
    ) -> JSValue;
    fn JS_GetGlobalObject(ctx: *mut JSContext) -> JSValue;
    fn JS_SetPropertyStr(
        ctx: *mut JSContext,
        this_obj: JSValue,
        prop: *const c_char,
        val: JSValue,
    ) -> c_int;
    fn JS_NewCFunction2(
        ctx: *mut JSContext,
        func: JSCFunction,
        name: *const c_char,
        length: c_int,
        cproto: c_int,
        magic: c_int,
    ) -> JSValue;
    // Shim wrappers for static inline functions
    #[link_name = "JS_NewString_shim"]
    fn JS_NewString(ctx: *mut JSContext, str: *const c_char) -> JSValue;
    #[link_name = "JS_ToCString_shim"]
    fn JS_ToCString(ctx: *mut JSContext, val: JSValue) -> *const c_char;
    #[link_name = "JS_FreeValue_shim"]
    fn JS_FreeValue(ctx: *mut JSContext, val: JSValue);

    // Non-inline functions
    fn JS_FreeCString(ctx: *mut JSContext, ptr: *const c_char);
    fn JS_GetException(ctx: *mut JSContext) -> JSValue;
    fn JS_SetContextOpaque(ctx: *mut JSContext, opaque: *mut c_void);
    fn JS_GetContextOpaque(ctx: *mut JSContext) -> *mut c_void;
    fn JS_NewObject(ctx: *mut JSContext) -> JSValue;
    fn JS_NewArray(ctx: *mut JSContext) -> JSValue;
    fn JS_SetPropertyUint32(ctx: *mut JSContext, this_obj: JSValue, idx: u32, val: JSValue) -> c_int;
}

// ---------------------------------------------------------------------------
// Emit callback data
// ---------------------------------------------------------------------------

struct VmOpaque {
    emit_sender: Sender<String>,
    query_sender: Sender<QueryRequest>,
}

// ---------------------------------------------------------------------------
// C callback: emit(turtle_string)
// ---------------------------------------------------------------------------

unsafe extern "C" fn js_emit(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: c_int,
    argv: *mut JSValue,
) -> JSValue {
    if argc < 1 {
        return js_undefined();
    }

    let val = *argv;
    let cstr = JS_ToCString(ctx, val);
    if cstr.is_null() {
        return js_undefined();
    }

    let s = CStr::from_ptr(cstr).to_string_lossy().into_owned();
    JS_FreeCString(ctx, cstr);

    let opaque = JS_GetContextOpaque(ctx) as *const VmOpaque;
    if !opaque.is_null() {
        let _ = (*opaque).emit_sender.send(s);
    }

    js_undefined()
}

// ---------------------------------------------------------------------------
// C callback: print(...)
// ---------------------------------------------------------------------------

unsafe extern "C" fn js_print(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: c_int,
    argv: *mut JSValue,
) -> JSValue {
    for i in 0..argc {
        let val = *argv.add(i as usize);
        let cstr = JS_ToCString(ctx, val);
        if !cstr.is_null() {
            let s = CStr::from_ptr(cstr).to_string_lossy();
            if i > 0 {
                eprint!(" ");
            }
            eprint!("{}", s);
            JS_FreeCString(ctx, cstr);
        }
    }
    eprintln!();
    js_undefined()
}

// ---------------------------------------------------------------------------
// C callback: store.query(sparql) → array of objects
// ---------------------------------------------------------------------------

unsafe extern "C" fn js_store_query(
    ctx: *mut JSContext,
    _this: JSValue,
    argc: c_int,
    argv: *mut JSValue,
) -> JSValue {
    if argc < 1 {
        return JS_NewArray(ctx);
    }

    let cstr = JS_ToCString(ctx, *argv);
    if cstr.is_null() {
        return JS_NewArray(ctx);
    }
    let sparql = CStr::from_ptr(cstr).to_string_lossy().into_owned();
    JS_FreeCString(ctx, cstr);

    let opaque = JS_GetContextOpaque(ctx) as *const VmOpaque;
    if opaque.is_null() {
        return JS_NewArray(ctx);
    }

    // Send query request and wait for response
    let (resp_tx, resp_rx) = mpsc::channel();
    if (*opaque).query_sender.send((sparql, resp_tx)).is_err() {
        return JS_NewArray(ctx);
    }

    match resp_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(rows) => {
            let arr = JS_NewArray(ctx);
            for (i, row) in rows.iter().enumerate() {
                let obj = JS_NewObject(ctx);
                for (var, val) in row {
                    if let Ok(key) = CString::new(var.as_str()) {
                        if let Ok(val_c) = CString::new(val.as_str()) {
                            let js_val = JS_NewString(ctx, val_c.as_ptr());
                            JS_SetPropertyStr(ctx, obj, key.as_ptr(), js_val);
                        }
                    }
                }
                JS_SetPropertyUint32(ctx, arr, i as u32, obj);
            }
            arr
        }
        Err(_) => JS_NewArray(ctx),
    }
}

// ---------------------------------------------------------------------------
// ScriptVm
// ---------------------------------------------------------------------------

pub struct ScriptVm {
    rt: *mut JSRuntime,
    ctx: *mut JSContext,
    _opaque: Box<VmOpaque>,
}

unsafe impl Send for ScriptVm {}

impl ScriptVm {
    pub fn new(emit_sender: Sender<String>, query_sender: Sender<QueryRequest>, memory_limit: usize) -> Result<Self> {
        let rt = unsafe { JS_NewRuntime() };
        if rt.is_null() {
            bail!("JS_NewRuntime failed");
        }

        if memory_limit > 0 {
            unsafe { JS_SetMemoryLimit(rt, memory_limit) };
        }

        let ctx = unsafe { JS_NewContext(rt) };
        if ctx.is_null() {
            unsafe { JS_FreeRuntime(rt) };
            bail!("JS_NewContext failed");
        }

        let opaque = Box::new(VmOpaque { emit_sender, query_sender });
        unsafe {
            JS_SetContextOpaque(ctx, &*opaque as *const VmOpaque as *mut c_void);
        }

        let global = unsafe { JS_GetGlobalObject(ctx) };

        unsafe {
            let name = CString::new("emit").unwrap();
            let func = JS_NewCFunction2(ctx, js_emit, name.as_ptr(), 1, 0, 0);
            JS_SetPropertyStr(ctx, global, name.as_ptr(), func);

            let name = CString::new("print").unwrap();
            let func = JS_NewCFunction2(ctx, js_print, name.as_ptr(), 1, 0, 0);
            JS_SetPropertyStr(ctx, global, name.as_ptr(), func);

            // Register store.query() — a JS object with a "query" method
            let store_obj = JS_NewObject(ctx);
            let qname = CString::new("query").unwrap();
            let qfunc = JS_NewCFunction2(ctx, js_store_query, qname.as_ptr(), 1, 0, 0);
            JS_SetPropertyStr(ctx, store_obj, qname.as_ptr(), qfunc);
            let sname = CString::new("store").unwrap();
            JS_SetPropertyStr(ctx, global, sname.as_ptr(), store_obj);

            JS_FreeValue(ctx, global);
        }

        Ok(Self {
            rt,
            ctx,
            _opaque: opaque,
        })
    }

    pub fn exec(&self, source: &str, input: &str, channel_uri: &str) -> Result<()> {
        let global = unsafe { JS_GetGlobalObject(self.ctx) };

        unsafe {
            let input_c = CString::new(input).unwrap_or_default();
            let input_val = JS_NewString(self.ctx, input_c.as_ptr());
            let prop = CString::new("input").unwrap();
            JS_SetPropertyStr(self.ctx, global, prop.as_ptr(), input_val);

            let channel_c = CString::new(channel_uri).unwrap_or_default();
            let channel_val = JS_NewString(self.ctx, channel_c.as_ptr());
            let prop = CString::new("channel").unwrap();
            JS_SetPropertyStr(self.ctx, global, prop.as_ptr(), channel_val);
        }

        // Wrap in IIFE so const/let don't pollute the global scope across repeated calls
        let wrapped = format!("(function(){{\n{}\n}})();", source);
        let source_c = CString::new(wrapped.as_str()).map_err(|_| anyhow!("script contains null byte"))?;
        let filename_c = CString::new("<script>").unwrap();

        let result = unsafe {
            JS_Eval(
                self.ctx,
                source_c.as_ptr(),
                wrapped.len(),
                filename_c.as_ptr(),
                JS_EVAL_TYPE_GLOBAL,
            )
        };

        if result.is_exception() {
            let exc = unsafe { JS_GetException(self.ctx) };
            let msg = unsafe {
                let cstr = JS_ToCString(self.ctx, exc);
                let s = if cstr.is_null() {
                    "unknown JS error".to_string()
                } else {
                    let s = CStr::from_ptr(cstr).to_string_lossy().into_owned();
                    JS_FreeCString(self.ctx, cstr);
                    s
                };
                JS_FreeValue(self.ctx, exc);
                s
            };
            unsafe { JS_FreeValue(self.ctx, global) };
            return Err(anyhow!("JS error: {}", msg));
        }

        unsafe {
            JS_FreeValue(self.ctx, result);
            JS_FreeValue(self.ctx, global);
        }

        Ok(())
    }
}

impl Drop for ScriptVm {
    fn drop(&mut self) {
        unsafe {
            JS_SetContextOpaque(self.ctx, std::ptr::null_mut());
            JS_FreeContext(self.ctx);
            // Skip JS_FreeRuntime — leaks ~100KB per VM but avoids
            // assert on GC objects (C functions on global). Acceptable
            // since VMs live until process exit.
        }
    }
}
