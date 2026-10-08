/* The smallest possible C surface over the fork's libllama.
 *
 * Everything else this crate needs from libllama -- llama_tokenize,
 * llama_detokenize, llama_vocab_* -- takes only plain pointers and integers,
 * so those are declared directly as `extern "C"` in Rust with no layout risk.
 *
 * `llama_model_params` is different. It is a by-value struct whose shape this
 * fork has already changed (`load_mode`, `lazy_mode`, `load_mtp` are not in
 * upstream), and a hand-transcribed copy in Rust would compile happily and
 * write `vocab_only` into whatever field happened to land at that offset. That
 * is a silent 200 GB weight load, or a crash, depending on the day. So the one
 * call that touches the struct goes through this file, which #includes the
 * fork's own header and therefore cannot disagree with it.
 *
 * This is linking, not vendoring: no llama.cpp source is copied here.
 */
#include "llama.h"

#include <stddef.h>

static void letibot_quiet_log(enum ggml_log_level level, const char * text, void * ud) {
    (void) level; (void) text; (void) ud;
}

/* Silence libllama's loader chatter and bring up the backend. Idempotent on the
 * llama side; the Rust wrapper additionally guards it with a Once. */
void letibot_llama_init(int quiet) {
    if (quiet) {
        llama_log_set(letibot_quiet_log, NULL);
    }
    llama_backend_init();
}

/* Load *only* the vocabulary out of a GGUF.
 *
 * `vocab_only` short-circuits the loader before any tensor is read, so this
 * needs no weights, no GPU and no running server -- measured at 0.6 s against
 * shard 1 of a six-way split 199 GB model, with the other five shards absent
 * from the call entirely.
 *
 * **`devices` names the CPU backend explicitly, and that is not cosmetic.** With
 * `devices = NULL` the loader takes its "default selection" branch, which
 * enumerates EVERY backend device and queries each one's properties -- and a
 * CUDA device's property query initializes that device's context. On a box whose
 * GPUs are already full (both of this fleet's carry ~97 GiB of 97.9), that
 * context creation fails, `ggml_cuda_set_device` raises `CUDA error: out of
 * memory`, and ggml ABORTS the process -- measured 2026-10-07, twice: once with
 * ~2.6 GiB free on GPU 1, where the same load succeeded, and again at ~0.8 GiB
 * free on GPU 0, where `cudaSetDevice(0)` died inside
 * `llama_prepare_model_devices`, before the `vocab_only` early exit was reached.
 * A vocabulary needs none of that: naming the CPU device skips the enumeration,
 * and the load then touches no GPU at all -- which is also what lets a daemon
 * already holding one model's weights load a SECOND model's vocabulary for a
 * `/models` switch without asking either GPU for anything.
 *
 * Returns NULL on failure; the caller turns that into a Rust error. */
struct llama_model * letibot_vocab_load(const char * path) {
    struct llama_model_params p = llama_model_default_params();

    /* The CPU device, and only it: a NULL-terminated list is the loader's
     * contract (llama.h, `devices`), and one entry plus the terminator is the
     * smallest list that says "auto-enumerate nothing". */
    ggml_backend_dev_t cpu_only[2] = {
        ggml_backend_dev_by_type(GGML_BACKEND_DEVICE_TYPE_CPU),
        NULL,
    };

    p.vocab_only              = true;
    p.n_gpu_layers            = 0;
    p.devices                 = cpu_only;
    p.check_tensors           = false;
    p.progress_callback       = NULL;
    p.progress_callback_user_data = NULL;

    return llama_model_load_from_file(path, p);
}
