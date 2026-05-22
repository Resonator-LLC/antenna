/*
 * antenna.h — C ABI for embedding Antenna in-process.
 *
 * Stable surface consumed by Dart bindings (and any other native embedder).
 * Mirrors antenna/src/ffi.rs 1:1; that file is authoritative for behavior,
 * this header is authoritative for the wire ABI.
 *
 * Wire format on both directions is RDF Turtle, identical to the WS and pipe
 * transports. The embedder is responsible for routing each emitted document
 * to whatever in-process consumer cares about it (Station's spatial canvas,
 * a script, a logger, ...).
 *
 * Copyright (c) 2026-2027 Resonator LLC. Licensed under MIT.
 */

#ifndef ANTENNA_H
#define ANTENNA_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle returned by antenna_create. Layout is private to Rust. */
typedef struct AntennaHandle AntennaHandle;

/*
 * Drain callback. `turtle` points at `len` UTF-8 bytes (NOT NUL-terminated)
 * owned by Antenna; the callee MUST NOT retain it past return.
 *
 * `user` is the same pointer the caller handed to antenna_drain.
 */
typedef void (*antenna_emit_cb)(void *user, const char *turtle, size_t len);

/*
 * Create an embedded Antenna instance.
 *
 * Arguments:
 *   data_dir            — required; libjami account/conversation directory.
 *   account_id_or_null  — Jami account ID to load, or NULL to mint a fresh one.
 *   store_dir_or_null   — Oxigraph store directory, or NULL for in-memory.
 *   pipeline_ttl_or_null — pipeline DAG Turtle CONTENT (not a path), or NULL.
 *   seed_ttl_or_null    — seed Turtle CONTENT (not a path), or NULL.
 *   out_account_id      — if non-NULL on success, *out_account_id is set to a
 *                         heap-allocated NUL-terminated UTF-8 string holding
 *                         the active account ID. Caller releases with
 *                         antenna_free(). Unchanged on failure.
 *
 * Returns the new handle on success, or NULL on failure (bad arguments, FFI
 * panic, libjami init failure, store/pipeline parse failure, etc.).
 */
AntennaHandle *antenna_create(const char *data_dir,
                              const char *account_id_or_null,
                              const char *store_dir_or_null,
                              const char *pipeline_ttl_or_null,
                              const char *seed_ttl_or_null,
                              char **out_account_id);

/*
 * Push one Turtle document onto the worker's IN ring. `len` bytes from
 * `turtle` are copied into the ring; `turtle` need not be NUL-terminated.
 *
 * Returns:
 *    0 — success
 *   -1 — invalid arguments (null handle, or null `turtle` with len > 0)
 *   -2 — bytes are not valid UTF-8
 *   -3 — ring buffer full after bounded retry (caller should drain + retry)
 */
int antenna_send(AntennaHandle *handle, const char *turtle, size_t len);

/*
 * Drain whatever Turtle documents are queued on the OUT ring, invoking `cb`
 * once per document. Returns the number of documents delivered, or -1 if the
 * handle is null. If `cb` is NULL, returns 0 without draining (the OUT-side
 * clock fd is still consumed, so a subsequent poll/select re-arms cleanly).
 */
int antenna_drain(AntennaHandle *handle, antenna_emit_cb cb, void *user);

/*
 * Return the read end of the OUT-side clock fd so callers can block in
 * poll/select/kqueue rather than busy-loop on antenna_drain. Returns -1 if
 * the handle is null or the clock fd is unavailable on this platform.
 */
int antenna_clock_fd(AntennaHandle *handle);

/*
 * Signal the worker thread to exit, join it, and release all resources
 * owned by the handle (including libjami via the dropped Antenna context).
 * Passing NULL is a no-op.
 */
void antenna_destroy(AntennaHandle *handle);

/*
 * Release a pointer previously handed out by an antenna_* function — at
 * present only `*out_account_id` from antenna_create. Passing NULL is a
 * no-op. Double-free is undefined behavior.
 */
void antenna_free(void *ptr);

#ifdef __cplusplus
}
#endif

#endif /* ANTENNA_H */
