# Guides

Two things ft-man does that are worth a walkthrough, because in both cases FreeToken has
no flag for what you want and the override is a file written into the checkpoint.

Both apply equally from the terminal UI and the browser one — the keys below are the TUI's;
the browser has the same actions as buttons.


## Using a different chat template

The **Templates** tab (`4`) fetches `.jinja` templates from any Hugging Face repo and
applies them to a checkpoint:

1. `r` → enter a repo (`peculiar-ragdoll/Qwen-Sharp-Chat-Templates` is the shipped
   default) → Enter to list its templates.
2. `f` on one to fetch it into the local store under `~/.local/state/ft-man/templates`.
3. Select the model on the **Models** tab, come back, and press `a`. The confirmation
   names every directory that will be written.
4. `v` renders the template against that model's real tokenizer — with a system prompt, a
   tool definition and a tool result — and reports the failure if it does not. This runs
   automatically before an apply unless you turn `templates.preflight` off; a template
   that fails to render breaks every request the engine serves, so it is worth the few
   seconds.
5. `u` restores the checkpoint's own template.

Because the engine reads its template when the model loads, **restart the engine** for a
change to take effect. The Models tab shows each checkpoint's current template, so an
override is never invisible.

Templates that take `chat_template_kwargs` (the Qwen-Sharp ones accept
`enable_thinking`, `tool_call_format`, `max_tool_arg_chars` and others) read them from
the request body — FreeToken passes `chat_template_kwargs` straight through from the
OpenAI and Anthropic APIs.

## Setting the sampling defaults

A request that names no `temperature` (or `top_p`, or `top_k`) still gets one. FreeToken
resolves the missing values from the checkpoint's `generation_config.json` — that is what
`--sampling-defaults=model`, its default, means — and falls back to temperature `0.0`,
top_k `-1`, top_p `1.0` when the checkpoint recommends nothing. The Dashboard's **Model
sampling** line reports what the running engine actually resolved to.

There is no `ft serve` flag for the values themselves, so to choose them ft-man changes what
that read finds. Select a checkpoint on the **Models** tab (`2`) and press `g`:

1. Fill in any of `temperature`, `top_p`, `top_k`. An empty field leaves the key out, so
   the engine uses its own default for that one value — which is not the same as a `0`.
2. Enter. The confirmation names every directory that will be written (the checkpoint, and
   its FTW build when there is one) and says so loudly when that is inside a shared Hugging
   Face cache, where other tools will see the change too.
3. `u` restores the checkpoint's own.

The values are merged into `generation_config.json`, never written over it: that file also
carries the stop token ids, and a model that loses those does not stop talking. The
original is kept beside it as `generation_config.json.ft-man-original`, so `u` puts back
exactly what shipped.

Two things worth knowing. Greedy decoding ignores `top_k` and `top_p` entirely, so setting
either without a temperature does nothing — ft-man says so before it writes. And a **GGUF**
checkpoint is refused outright: it carries its sampling in the file's own metadata, which
FreeToken reads first, so a `generation_config.json` beside it would be written and then
never read.

As with a template, the engine reads this once at load time, so **restart the engine** for
a change to take effect.

