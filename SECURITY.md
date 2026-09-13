# Security

## Reporting a vulnerability

Please use GitHub's [private vulnerability
reporting](https://github.com/jlbyh2o/ft-man-tui/security/advisories/new) rather than opening
a public issue. There is no security contact email.

## The engine ft-man drives has no authentication

This is the most important thing to know about running FreeToken anywhere but a machine you
alone can reach.

`ft serve` has no authentication of any kind. There is no API key flag, and its OpenAI-,
Anthropic- and Responses-compatible routes are registered with no auth dependency. Anything
that can open a TCP connection to the engine's port can spend your GPU, read the prompts you
send it, and generate whatever it likes.

ft-man defaults `--host` to `0.0.0.0` because the common case is a machine deliberately sat
beside a GPU on a trusted network. That default is wrong the moment the host has a public
address.

**On a rented or cloud GPU, do not publish the engine's port.** Two safe shapes:

- **SSH tunnel.** Leave the port unmapped and forward it:
  `ssh -N -L 1919:127.0.0.1:1919 <host>`, then talk to `127.0.0.1:1919` locally.
- **Authenticating proxy.** Bind the engine to `127.0.0.1` and put something in front that
  requires credentials. The container in `docker/` does this with the Vast.ai Instance
  Portal's Caddy layer; see `docker/README.md`.

## The web interface has no authentication by default either

`ft-man web` serves the same control surface the TUI has, over HTTP, to whoever can reach
the port: it can start and stop the engine, delete checkpoints, write files (an FTW
conversion, a chat template override) and read every log line and request in the ring.
Authentication is an opt-in bearer token (`[web] token` / `--token`) checked against
`Authorization: Bearer` or a cookie the login page sets; with no token configured, anything
that can open a TCP connection to the port has the whole surface, which is the same
no-auth-by-default `ft serve` already has. There is no TLS — traffic, including the token
if one is set, is plaintext.

Treat it the same way as the engine: fine on a machine only you can reach, wrong the moment
the host has a public address. Bind `[web] listen` to `127.0.0.1` and reach it over an SSH
tunnel, or set a token and put an authenticating, TLS-terminating reverse proxy in front of
it — do not publish port 7979 directly to the internet.

## Tokens

ft-man reads a Hugging Face token from `HF_TOKEN`, `HUGGING_FACE_HUB_TOKEN`, `hub.token` in
its config file, or the token cached by the `hf` CLI, in that order. It sends that token only
to the configured Hub endpoint (`https://huggingface.co` unless you change it). It is never
written to logs, and never to a container image — `docker/scan-image.sh` fails the build if a
credential-shaped string reaches one.
