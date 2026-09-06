# freetoken-ftman container

FreeToken's MoE serving engine and the `ft-man` terminal UI in one image, for running on
a rented NVIDIA GPU.

## What is pinned

| | |
|---|---|
| Base | builders on `nvidia/cuda:13.0.3-devel-ubuntu24.04`; final stage on `13.0.3-runtime-ubuntu24.04` plus `cuda-nvcc`, `cuda-cudart-dev` and `g++`, which restores the JIT fallback at a fraction of devel's size |
| FreeToken | upstream `FlashML-org/FreeToken` at `af71ba4`, plus `freetoken-nvfp4-moe.patch` |
| ft-man | built from this repository's working tree |
| Python | 3.12 (the base image's), venv at `/opt/freetoken/venv` |
| Rust | `rust:1.98.0-slim-bookworm` — bookworm's older glibc so the binary runs on the ubuntu24.04 final stage |

`uv` is installed unpinned from `astral.sh/uv/install.sh`, which is the one input to this
build that is not version-locked.

### The patch

`freetoken-nvfp4-moe.patch` teaches the Qwen3.5-MoE family to recognize compressed-tensors
NVFP4 exports. Without it, `ft checkpoint` on such a checkpoint runs for minutes, writes
most of the model, then dies with `Missing MoE expert source layers`. See
`../docs/freetoken-compressed-tensors-moe.md` for what it does and why.

It is a `git diff`, not a `git format-patch`: the latter embeds an author header, and the
whole point is that the image carries the change without carrying its provenance. It
applies to `af71ba4` and no other tree — if you move `FREETOKEN_SHA`, expect to regenerate
it.

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
| Launch mode | SSH |
| On-start script | `bash /opt/ft/onstart.sh` |
| Ports | `-p 1919:1919` |
| Environment | `HF_TOKEN=<token>`, only for gated or private repos |

The on-start line is not optional in SSH mode. Vast's SSH and Jupyter launch modes replace
the image's entrypoint with their own startup, so `entrypoint.sh` never runs and the setup
it would have done has to be requested explicitly.

This image deliberately ships **no sshd**. Vast injects and runs its own, and an image
that competes with it is a known cause of instances you cannot log into.

Once you are in:

```bash
ft-man --doctor     # what it found: the ft binary, the GPU, your checkpoints
ft-man              # the UI
```

### Why the on-start script exists

Beyond creating directories, it bridges the instance environment into your login shell.
Vast's documentation is explicit that variables you set at instance creation are **not**
visible inside SSH, tmux or Jupyter sessions. `HF_TOKEN` arrives exactly that way, so
without the bridge `ft-man` reports no token on an instance you gave one to, and gated
downloads fail with a 401 and no obvious cause.

It writes the token to `/etc/profile.d/10-freetoken.sh` inside the running container. That
is the filesystem of a machine you rented, never a layer of the published image.

## Weights, on destroy-after-use rentals

Nothing about the model is in the image. A 23 GiB checkpoint download is the cheap half of
the problem; the FTW conversion is the expensive half, and both die with the instance.

Convert once, then keep the FTW build somewhere you control:

```bash
# first rental only
ft-man                                  # Hub tab: download; Jobs tab: convert to FTW
hf upload <you>/<model>-ftw /workspace/models/<model>-ftw --repo-type model --private

# every rental after
hf download <you>/<model>-ftw --local-dir /workspace/models/<model>-ftw
```

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
   `freetoken` explicitly, since that is the one package this build patches.
4. **Authored files** — the paths this build wrote, scanned with weak patterns too
   (a bare first name, a mail domain), where a hit is meaningful. Plus a check for stray
   `.git` directories, `authorized_keys`, `.netrc`, `.docker/config.json` and cached
   HF tokens.

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
| NVFP4 patch (shipped module) | exercised against the installed package, not the source tree: a compressed-tensors MoE export resolves to `nvfp4`, a dense one to `none`, and MXFP4 geometry raises rather than misrouting into the NVFP4 loader |
| NVFP4 patch (own test suite) | `tests/models/test_qwen3_5_moe_config.py` — 13 passed, run against the patched tree in the builder stage |
| Env bridge | `HF_TOKEN` recovered in login *and* interactive shells with it absent from the child environment |
| Scan | clean |
| Size | 18.4 GB on disk, 6.38 GB compressed |

Not verified: an actual model load or serve. The local card holds 8 GB, and the target
checkpoint is 23 GiB, so the first real serve necessarily happens on rented hardware.

## Known follow-ups

- **Size: done, twice.** 32.91 GB / 9.98 GB compressed at first build, now **18.4 GB /
  6.38 GB compressed** — a 36% cut in what a Vast host pulls. Two removals, both verified
  against a real GPU rather than assumed:

  | Cut | On disk | Compressed |
  |---|---|---|
  | flashinfer cubins (Blackwell datacenter only) | −8.2 GB | −1.52 GB |
  | devel base → runtime + `cuda-nvcc` | −6.3 GB | −2.08 GB |

  The cubins were exclusively sm100a/sm103a/sm107a/sm100f. Nothing in them served sm_89,
  sm_120, sm_80 or sm_90a — those come from `flashinfer_jit_cache`, whose 906 `.so` files
  are fat binaries carrying all six architectures. The cost is the TensorRT-LLM fast paths
  on B200-class hardware, where flashinfer JITs instead; `--build-arg FLASHINFER_CUBINS=keep`
  restores them.

  The devel base carried 2.56 GB of static `.a` libraries nothing links against, plus
  compute-sanitizer and full headers. A real FreeToken kernel
  (`freetoken__store_1024_128_1_false`) compiles in ~3 s on either base, so the JIT
  fallback survived the swap intact.

- **What is left, and why it was not taken.** `nccl` (209 MB), `cusparselt` (223 MB) and
  `nvshmem` (78 MB) are multi-GPU and sparse libraries that single-GPU MoE inference
  probably never loads — worth ~0.2 GB compressed. "Probably" is the problem: unlike the
  two cuts above, this one needs a real serve to disprove, and a lazily dlopened library
  that goes missing fails at load time on rented hardware. Bad trade at that price.

- **FreeToken's own kernel-cache wheel.** `flashinfer-cubin` and `flashinfer-jit-cache`
  already remove the flashinfer JIT stall. FreeToken's `scripts/build-release-wheels.sh`
  builds an equivalent cache for its own TVM FFI kernels; whether it needs a GPU present
  to build is untested.
