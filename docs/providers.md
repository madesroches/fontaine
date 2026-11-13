# Providers

An account is `[accounts.<name>]` in your user settings. Models are addressed `<account>/<model>`.
This page helps you choose a kind and shows a minimal example of each;
[Configuration](configuration.md#accountsname) is the key reference.

## Choosing a kind

| Kind | Service | Wire | Endpoint | Credential | Pricing | Notable |
|---|---|---|---|---|---|---|
| `anthropic` | Anthropic API or a host of it | Messages | `https://api.anthropic.com` | required | priced | prompt caching, native web search |
| `openai` | OpenAI API | Responses | `https://api.openai.com/v1` | required | priced | |
| `gemini` | Gemini API | `generateContent` | `https://generativelanguage.googleapis.com` | required | priced | |
| `openai-compatible` | Any Chat Completions server | Chat Completions | `base_url` required | optional | unpriced | `quota = "kimi"` |
| `kimi` | Kimi for Coding | Chat Completions | `https://api.kimi.com/coding/v1` | optional | unpriced | quota display |
| `ollama` | Local Ollama | `/api/chat` | `http://localhost:11434` | optional | priced if catalog lists it | `context_tokens`, `think` |
| `opencode-go` | OpenCode Go | four wires, per model | `https://opencode.ai/inference/go` | sign-in | unpriced | `wires` |
| `databricks` | Databricks AI Gateway | four wires, per model | `base_url` required | required (Bearer) | unpriced | `wires`, `model_facts` |
| `openrouter` | OpenRouter | Chat Completions | `https://openrouter.ai/api/v1` | required | priced | prompt caching |
| `claude-subscription` | Claude Max or Ultra via the Claude CLI | Messages, via the CLI | none | the CLI's login | unpriced | native web search, plan quota |

`--max-budget-usd` does not apply to unpriced models, and with one as the main model it is a usage
error. Native web search is offered for `anthropic` and `claude-subscription`
only; see [`[websearch]`](configuration.md#websearch).

## Easily confused choices

- **`kimi` or `openai-compatible`.** `kimi` is the flat-rate Kimi for Coding subscription, with
  quota built in. Moonshot's pay-as-you-go API is `openai-compatible`.
- **`openrouter` or `openai-compatible` at OpenRouter.** Only `openrouter` sends cache markers.
- **`databricks` or `anthropic` with `auth = "bearer"`.** `databricks` is for a workspace and
  routes each model to its wire. Bearer auth on `anthropic` is for other Messages-only gateways.
- **One wire or several.** `opencode-go` and `databricks` pick the wire per model id; fix a wrong
  pick with `wires`. The other kinds have one wire.
- **`claude-subscription`.** No Fontaine credential: the Claude CLI owns the Max or Ultra login.
  For API billing use `anthropic`. Scope and terms: [Claude subscription](claude-subscription.md).

## `anthropic`

Messages API. `base_url` points at another host. Key from `api_key_env` or `/login`; `auth =
"bearer"` sends it as a Bearer token. `prompt_caching` (default on) and `cache_ttl` apply.
`model_facts` maps a host's own model names to catalog models.

```toml
[accounts.work]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[model]
model = "work/claude-sonnet-5"
```

## `openai`

Responses API. Key from `api_key_env` or `/login`.

```toml
[accounts.openai]
kind = "openai"
api_key_env = "OPENAI_API_KEY"
models = ["gpt-5.2", "gpt-4o-mini"]

[model]
model = "openai/gpt-5.2"
```

## `gemini`

Native `generateContent`. Key from `api_key_env` or `/login`.

```toml
[accounts.google]
kind = "gemini"
api_key_env = "GEMINI_API_KEY"

[model]
model = "google/gemini-2.5-pro"
```

## `openai-compatible`

Chat Completions, not Responses (LM Studio, vLLM, LiteLLM). `base_url` is required; no key means
no `Authorization` header. Unlisted models never get derived facts.

```toml
[accounts.lmstudio]
kind = "openai-compatible"
base_url = "http://localhost:1234/v1"

[model]
model = "lmstudio/qwen2.5-coder-7b-instruct"
```

## `kimi`

Kimi for Coding over Chat Completions. Its quota shows in the [status bar](tui.md#status-bar)
with no `quota` key.

```toml
[accounts.kimi]
kind = "kimi"
api_key_env = "KIMI_API_KEY"
```

## `ollama`

Local daemon, native `/api/chat`, no key needed. `context_tokens` (default `16384`) is sent with
every call; `think = false` skips reasoning.

```toml
[accounts.local]
kind = "ollama"
base_url = "http://localhost:11434"
context_tokens = 16384

[model]
model = "local/qwen3:4b"
```

## `opencode-go`

No key: sign in with `/login <account>`. `wires` overrides the built-in routing. `prompt_caching`
defaults to off.

```toml
[accounts.go]
kind = "opencode-go"

[accounts.go.wires]
"some-new-model" = "messages"
```

## `databricks`

`base_url` is the workspace origin, no path. `claude-` models use the Messages route, `gpt-` the
Responses route, `gemini-` the Gemini route, the rest MLflow Chat Completions. Billed in DBUs.
`model_facts` maps a custom endpoint; it does not change routing. See
[Configuration](configuration.md#worked-examples).

```toml
[accounts.dbx]
kind = "databricks"
base_url = "https://example.cloud.databricks.com"
api_key_env = "DATABRICKS_TOKEN"

[model]
model = "dbx/system.ai.claude-sonnet-5-5"
```

## `openrouter`

Model ids carry the upstream (`router/anthropic/claude-sonnet-4-6`). `prompt_caching` defaults on,
except for the self-caching `openai/`, `deepseek/`, `x-ai/` and `groq/` upstreams.

```toml
[accounts.router]
kind = "openrouter"
api_key_env = "OPENROUTER_API_KEY"
models = ["anthropic/claude-sonnet-4-6"]

[model]
model = "router/anthropic/claude-sonnet-4-6"
```

## `claude-subscription`

Runs your installed Claude CLI as the transport. Max and Ultra plans only, for individual local
use; check Anthropic's terms. Sign in with `claude auth login --claudeai`. Tokens show, cost
never does. Checks, limits and failures: [Claude subscription](claude-subscription.md).

```toml
[accounts.claude]
kind = "claude-subscription"
```
