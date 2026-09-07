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

## Tokens

ft-man reads a Hugging Face token from `HF_TOKEN`, `HUGGING_FACE_HUB_TOKEN`, `hub.token` in
its config file, or the token cached by the `hf` CLI, in that order. It sends that token only
to the configured Hub endpoint (`https://huggingface.co` unless you change it). It is never
written to logs, and never to a container image — `docker/scan-image.sh` fails the build if a
credential-shaped string reaches one.
