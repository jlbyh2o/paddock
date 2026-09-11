# freetoken-ftman container

FreeToken's MoE serving engine and the `ft-man` terminal UI in one image, for running on
a rented NVIDIA GPU.

## What is pinned

| | |
|---|---|
| Base | builders on `nvidia/cuda:13.0.3-devel-ubuntu24.04`; final stage on `vastai/base-image:cuda-13.0.3-cudnn-devel-ubuntu24.04-py312-2026-08-28`, which already carries `nvcc` and `g++` for the JIT fallback |
| FreeToken | upstream `FlashML-org/FreeToken` at `0ffd5c8`, unmodified |
| ft-man | built from this repository's working tree |
| Python | 3.12 (the base image's), venv at `/opt/freetoken/venv` |
| Rust | `rust:1.98.0-slim-bookworm` — bookworm's older glibc so the binary runs on the ubuntu24.04 final stage |

`uv` is installed unpinned from `astral.sh/uv/install.sh`, which is the one input to this
build that is not version-locked.

### No patch

Earlier images carried `freetoken-nvfp4-moe.patch`, which taught the Qwen3.5-MoE family to
recognize compressed-tensors NVFP4 exports. FreeToken #418/#427/#438 replaced every
family-local quantization detector with a single `QuantConfig` layer that reads each
module's scheme by name in either dialect, which covers that case and more, so the patch
was dropped and `FREETOKEN_SHA` moved to `0ffd5c8`. See
`../docs/freetoken-compressed-tensors-moe.md` for the history.

## Build

```bash
docker/build.sh                                    # build + scan, no push
DOCKER_NAMESPACE=<account> docker/build.sh --push  # build + scan + push
```

Tags come from what is actually inside the built image — `ft0.1.2-man0.1.0-cu13` — read
back out of it after the build rather than assumed beforehand, so a tag cannot claim a
version the image does not have. `:latest` moves with every push.

Your Docker Hub account name is read from `DOCKER_NAMESPACE` and never written to a file.
Nothing in this repository records where the image is published.

## Running on Vast.ai

Rent a host with **driver r580 or newer**. CUDA 13 requires it, many Vast hosts are older,
and the failure arrives at weight load rather than at boot. Filter offers on CUDA version.

| Field | Value |
|---|---|
| Image | `<account>/freetoken-ftman:latest` |
| Launch mode | **Jupyter-python notebook + SSH** (or Interactive shell server) |
| On-start script | `entrypoint.sh` |
| `PORTAL_CONFIG` | append `localhost:18919:1919:/:FreeToken API` to the template's value |
| Ports | **do not map 1919** — see below |
| Environment | `HF_TOKEN=<token>`, only for gated or private repos |

**Pick a launch mode that gives you SSH.** The base image contains no sshd; SSH exists only
because Vast injects one, which it does in the Jupyter and Interactive-shell modes and not in
Entrypoint mode. Entrypoint mode would leave you reaching the box only through the portal's
browser terminal.

**Put `entrypoint.sh` in the on-start field.** These modes replace the image's `ENTRYPOINT`
with Vast's own startup, so the base image's boot sequence does not run on its own — you
re-invoke it. `entrypoint.sh` resolves on `PATH` to `/opt/instance-tools/bin/entrypoint.sh`,
which is exactly what this image's inherited `ENTRYPOINT` points at, so the on-start field
runs the same thing Entrypoint mode would have. This is what Vast's own templates do.

It then walks `/etc/vast_boot.d/` — propagating SSH keys, exporting the instance environment,
generating a TLS certificate, and launching supervisor, which starts Caddy, the Instance
Portal and this image's `freetoken-setup` program. The boot scripts read the `/.launch` file
Vast writes to tell which mode they are in and adapt: `jupyter.sh` stands down when Vast is
managing Jupyter, and `10-prep-env.sh` adds or strips the Jupyter portal entries to match.
Leave the field empty and you get an instance with SSH but no portal, no Caddy, and no
`/workspace` layout.

### Do not expose port 1919

`ft serve` has no authentication. There is no `--api-key` flag, and the OpenAI, Anthropic
and Responses routes are registered with no auth dependency — the only `API_KEY` strings in
FreeToken are in `launch.py`, for pointing outbound clients at the engine. Mapping 1919 on
a Vast instance therefore publishes an unauthenticated inference endpoint on a public IP:
anyone who finds it can spend your rented GPU, read what you send it, and generate whatever
they like on your bill.

Reach the engine over an SSH tunnel instead, which needs no port mapping at all:

```bash
ssh -N -L 1919:127.0.0.1:1919 <vast-ssh-target>
# then, locally
curl http://127.0.0.1:1919/v1/models
```

The engine still binds `0.0.0.0` inside the container so the tunnel and ft-man's own polling
both reach it; what changes is that Vast never publishes the port. On this base you need not tunnel at all: Caddy already fronts the engine. Add
`localhost:18919:1919:/:FreeToken API` to the instance's `PORTAL_CONFIG` and the API appears
in the Instance Portal on 18919, behind the portal's TLS and authentication. `PORTAL_CONFIG`
is deliberately not baked into the image — it comes from the template, and a baked value
would silently drop the base image's own entries.

This image ships **no sshd of its own** — and neither does the base, which leaves Vast to
inject one as usual.

Once you are in:

```bash
ft-man --doctor     # what it found: the ft binary, the GPU, your checkpoints
ft-man              # the UI
```

### What the setup program does

It creates the `/workspace` layout and writes the ft-man config, then bridges the instance
environment into login shells via `/etc/profile.d/10-freetoken.sh`.

On this base that bridge is belt-and-braces: `10-prep-env.sh` already writes the instance
environment to `/etc/environment` and `45-user-write-bashrc.sh` sources it from `.bashrc`,
so `HF_TOKEN` reaches your shell without help. The bridge still earns its keep when the
image runs outside Vast, where none of that boot sequence exists.

Either way the token is written only to the filesystem of a machine you rented, never into
a layer of the published image.

## Weights, on destroy-after-use rentals

Nothing about the model is in the image. A 23 GiB checkpoint download is the cheap half of
the problem; the FTW conversion is the expensive half, and both die with the instance.

Convert once, then keep the FTW build somewhere you control:

```bash
# first rental only
ft-man                                  # Hub tab: pick a quantization and download;
                                        # Jobs tab: convert to FTW
hf upload <you>/<model>-ftw /workspace/models/<dir> --repo-type model --private

# every rental after
hf download <you>/<model>-ftw --local-dir /workspace/models/<dir>
```

`<dir>` is what ft-man named the build, shown on the Models tab: the repo id with `/`
replaced by `--`, plus the quantization when the repo ships more than one — so
`unsloth--Qwen3.8-Flash-Next-GGUF--UD-IQ3_XXS-ftw`. The organization and quantization are
in the name because two organizations publish the same model name often enough, and two
quantizations of one repo would otherwise convert into the same directory.

`hf` is already on `PATH` — `huggingface_hub` is a FreeToken dependency, so the CLI comes
along with the engine. It reads the same `HF_TOKEN` the bridge exports.

## What the scan checks

`docker/scan-image.sh <image>` runs in three passes and exits non-zero on any hit.
`build.sh` treats that as fatal: a false positive costs a minute, a miss is public,
mirrored, and cached forever.

1. **Metadata** — `docker inspect` and the full `docker history`. Build arguments and
   labels travel with the image to everyone who pulls it. Also fails on any
   credential-shaped variable in the image environment.
2. **Identity, whole filesystem** — every byte of the image streamed through `grep` in one
   pass, no scratch space. Only strings that cannot legitimately appear anywhere:
   usernames and host paths.
3. **Credentials, everything this build wrote** — token shapes, private-key headers, cloud
   key ids. Scanned over the whole filesystem *except* third-party `site-packages`, plus
   `freetoken` explicitly, since that is the one package this build installs from source.
4. **Authored files** — the paths this build wrote, scanned with weak patterns too
   (a bare first name, a mail domain), where a hit is meaningful. Plus a check for stray
   `.git` directories, `authorized_keys`, `.netrc`, `.docker/config.json` and cached
   HF tokens.

Passes 3 and 4 are **differential**: the same checks run against the base image and only
new findings fail. Set `BASE_IMAGE` to override the base, which is otherwise read from the
`ARG VASTAI_IMAGE=` line in the Dockerfile. With no base image available locally the checks
run absolute, which fails closed rather than passing silently.

### Why differential

Building on `vastai/base-image` means inheriting things that look exactly like findings and
are not: an empty `/root/.ssh` that Vast fills at boot, git checkouts of `vast-cli` and
`nvm`, and CPython's stdlib test certificates under `/usr/lib/python3.12/test/certdata/`.
None came from this build. An allowlist would have to be re-audited every time Vast changes
their image; a diff against the actual base cannot rot.

Detection is unaffected. Verified against a fixture built *on* the real base with a private
key, an authorized_keys, a token file and a host path planted in it: all five checks still
fail. A non-empty `/root/.ssh` is a finding precisely because the base ships it empty.

### Why site-packages is excluded from the credential pass

Upstream packages legitimately ship credential-shaped constants. `cryptography`'s SSH
parser holds `-----BEGIN OPENSSH PRIVATE KEY-----` as a literal, and `transformers` ships
a public sandbox CI token in `testing_utils.py`. Both are in every copy of those packages
on PyPI, identical for every user, and both fail a naive pattern scan.

The trust boundary is authorship, not pattern strength. This build writes no files into
third-party packages, so a credential of yours cannot land in one. Muting the patterns
instead would have weakened the check everywhere; scoping them keeps them absolute where
they matter.

Weak patterns are kept out of the whole-filesystem pass for the same class of reason:
33 GB of third-party wheels contains a great many Jeremys who are not you.

Deliberately absent from the image: any maintainer or author label, any
`org.opencontainers.image.source`, the fork remote, host paths, and `HF_TOKEN`.

## Verified

On an RTX 4060 Laptop (sm_89, driver 610.57.04), inside the built image:

| | |
|---|---|
| `ft` / `ft-man` | freetoken 0.1.2 / ft-man 0.1.0 |
| `ft-man --doctor` | resolves the venv via config, `/workspace` paths, and NVML (names the GPU, VRAM and UUID) |
| torch | 2.11.0+cu130, `cuda.is_available()` true, bf16 matmul on device returns finite values |
| accel stack | `flashinfer` 0.6.18.post1, `flashinfer_cubin`, `flashinfer_jit_cache`, `sgl_kernel` all import; cubins resolve to the packaged directory |
| nvcc JIT fallback | compiles and runs a real CUDA kernel on the GPU inside the final image |
| FreeToken JIT | `freetoken__store_1024_128_1_false` compiles via `load_jit` in ~4 s on the runtime base — the fallback the `cuda-nvcc` package exists for |
| NVFP4 quant config (shipped module) | exercised against the installed package, not the source tree: a compressed-tensors MoE export resolves to `nvfp4`, a dense one to `none`, and MXFP4 geometry raises rather than misrouting into the NVFP4 loader |
| Env bridge | `HF_TOKEN` recovered in login *and* interactive shells with it absent from the child environment |
| Scan | clean |
| Size | 30.9 GB on disk, 10.14 GB compressed total — **4.46 GB incremental** over the cached base |

Not verified: an actual model load or serve. The local card holds 8 GB, and the target
checkpoint is 23 GiB, so the first real serve necessarily happens on rented hardware.

## Known follow-ups

- **Size.** 30.9 GB on disk, 10.14 GB compressed in total, of which 5.68 GB is
  `vastai/base-image` — leaving **4.46 GB that actually pulls** on a host that already has
  the base cached, which Vast hosts reliably do.

  | Stage | On disk | Compressed | Effective pull |
  |---|---|---|---|
  | First build | 32.91 GB | 9.98 GB | 9.98 GB |
  | Cubins pruned | 24.7 GB | 8.46 GB | 8.46 GB |
  | Runtime base | 18.4 GB | 6.38 GB | 6.38 GB |
  | Rebased on vastai | 30.9 GB | 10.14 GB | **4.46 GB** |

  The rebase makes the image larger and the pull smaller, which is only true because the
  base is cached. Standalone it would be the worst of the four.

  Two cuts still hold inside it. flashinfer's cubins were exclusively sm100a/sm103a/sm107a/
  sm100f — Blackwell datacenter — while sm_89, sm_120, sm_80 and sm_90a all come from
  `flashinfer_jit_cache`'s fat binaries; `--build-arg FLASHINFER_CUBINS=keep` restores them
  for B200 work. The runtime-base experiment is no longer in the tree, but it established
  that FreeToken's JIT needs only `nvcc`, `cuda-cudart-dev` and `g++` — worth remembering if
  Vast ever ships a non-devel variant.

- **What is left, and why it was not taken.** `nccl` (209 MB), `cusparselt` (223 MB) and
  `nvshmem` (78 MB) are multi-GPU and sparse libraries that single-GPU MoE inference
  probably never loads — worth ~0.2 GB compressed. "Probably" is the problem: unlike the
  two cuts above, this one needs a real serve to disprove, and a lazily dlopened library
  that goes missing fails at load time on rented hardware. Bad trade at that price.

- **FreeToken's own kernel-cache wheel.** `flashinfer-cubin` and `flashinfer-jit-cache`
  already remove the flashinfer JIT stall. FreeToken's `scripts/build-release-wheels.sh`
  builds an equivalent cache for its own TVM FFI kernels; whether it needs a GPU present
  to build is untested.
