# syntax=docker/dockerfile:1

# FreeToken's MoE serving engine plus the ft-man terminal UI, ready to run on a rented
# NVIDIA GPU.
#
# Nothing here identifies who built it: no maintainer label, no source repository, no
# host paths, no credentials, and no fork remotes. docker/scan-image.sh proves that
# against the built image, and docker/build.sh refuses to push when it finds anything.

# Builders need the full toolkit (they compile FreeToken's three C++ extensions against
# libtorch). The final stage does not: it needs nvcc for the JIT fallback and nothing else
# the devel image adds, so it starts from runtime and installs the compiler back. Measured:
# devel is 3.97 GB compressed, runtime 1.65 GB, and the packages added below cost ~0.2 GB.
ARG CUDA_IMAGE=nvidia/cuda:13.0.3-devel-ubuntu24.04
ARG RUST_IMAGE=rust:1.98.0-slim-bookworm
# Vast's official base. Larger than a bare CUDA runtime, but Vast hosts keep these layers
# cached -- observed pulling near-instantly on real rentals -- so the incremental pull is
# our layers alone. It also brings the Instance Portal, and Caddy in front of the engine
# with TLS and authentication, which matters because `ft serve` has none of its own.
# Its CUDA is 13.0.3 and its /usr/bin/python3 is 3.12, matching what the venv was built for.
ARG VASTAI_IMAGE=vastai/base-image:cuda-13.0.3-cudnn-devel-ubuntu24.04-py312-2026-08-28


# --- ft-man ------------------------------------------------------------------------
# Built on bookworm (glibc 2.36) deliberately: the binary has to run on the ubuntu24.04
# final stage (glibc 2.39), and glibc is only forward compatible. Building on the newer
# base and running on the older one is what breaks.
FROM ${RUST_IMAGE} AS ft-man-builder

WORKDIR /build

# Dependencies compile from a stub main so that editing ft-man rebuilds one crate rather
# than the whole tree. Nothing but Cargo.toml and Cargo.lock is in the context yet.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
 && echo 'fn main() {}' > src/main.rs \
 && cargo build --release --locked \
 && rm -rf src target/release/ft-man target/release/deps/ft_man-*

COPY src ./src
# cargo compares mtimes, and COPY can land a file older than the stub build; touch makes
# the rebuild unconditional so the stub can never survive into the image.
RUN touch src/main.rs && cargo build --release --locked && /build/target/release/ft-man --version


# --- FreeToken ---------------------------------------------------------------------
FROM ${CUDA_IMAGE} AS freetoken-builder

ARG FREETOKEN_REPO=https://github.com/FlashML-org/FreeToken.git
# The exact commit the NVFP4 MoE fix was written and verified against. Pinned rather than
# tracking main: the patch below applies to this tree and no other.
ARG FREETOKEN_SHA=af71ba43206e124f5ff6419b47ee36c6e9981078

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential ca-certificates curl git ninja-build \
      python3 python3-dev python3-venv \
 && rm -rf /var/lib/apt/lists/*

# The wheels this pulls are cp312. A base image that moved off 3.12 would install a
# resolvable but wrong set, so fail here instead of at import time on a rented GPU.
RUN python3 -c "import sys; assert sys.version_info[:2] == (3, 12), f'need python 3.12, base image has {sys.version}'"

ENV CUDA_HOME=/usr/local/cuda
ENV PATH=/usr/local/cuda/bin:${PATH}
ENV VENV=/opt/freetoken/venv

RUN curl -LsSf https://astral.sh/uv/install.sh | UV_UNMANAGED_INSTALL=/usr/local/bin sh \
 && uv --version

# --python is the absolute system interpreter on purpose: given a bare "3.12" uv may
# download and link a managed CPython under ~/.local/share/uv, which the final stage
# does not copy, leaving a venv whose interpreter symlink dangles.
RUN uv venv "$VENV" --python /usr/bin/python3

# Upstream, at the pinned commit, plus the fix as a patch. Cloned from upstream rather
# than from a fork so the image carries the change without carrying its provenance.
# A depth-1 fetch of one SHA rather than a full clone: ~40 MB instead of ~400 MB.
RUN mkdir -p /src/freetoken \
 && cd /src/freetoken \
 && git init -q \
 && git remote add origin "$FREETOKEN_REPO" \
 && git fetch -q --depth 1 origin "$FREETOKEN_SHA" \
 && git checkout -q FETCH_HEAD

COPY docker/freetoken-nvfp4-moe.patch /tmp/freetoken-nvfp4-moe.patch
RUN git -C /src/freetoken apply --verbose /tmp/freetoken-nvfp4-moe.patch

# Provenance pins for the two packages whose cu130 builds live off PyPI, plus flashinfer's
# own index. Copied from upstream's install.sh, including the reasoning: unsafe-best-match
# is required because the pytorch index mirrors stale copies of common dependencies that
# would otherwise shadow PyPI under uv's first-index strategy. All four are trusted.
# [tool.uv.sources] in pyproject.toml does not survive into a build, so it is repeated.
ARG CU_INDEXES="--index-strategy unsafe-best-match --extra-index-url https://download.pytorch.org/whl/cu130 --extra-index-url https://docs.sglang.io/whl/cu130 --extra-index-url https://flashinfer.ai/whl --extra-index-url https://flashinfer.ai/whl/cu130"

# torch goes in first, from the cu130 channel, so setup.py's three C++ extensions
# (_pinned_tensor, _cpu_moe, _ple_store) link against exactly the libtorch that will be
# present at runtime. Building under isolation would resolve a second torch and risk the
# mismatch pyproject.toml warns about.
RUN uv pip install --python "$VENV" ${CU_INDEXES} \
      "torch>=2.11,<2.12" "setuptools>=77" wheel ninja

# flashinfer-cubin and flashinfer-jit-cache ship flashinfer's kernels prebuilt and
# multi-arch. Without them flashinfer JIT-compiles on first use, which is an nvcc stall
# on metered hardware. ~2 GB, downloaded once, at build time instead of at serve time.
RUN uv pip install --python "$VENV" --no-build-isolation ${CU_INDEXES} \
      "/src/freetoken[accel]" flashinfer-cubin flashinfer-jit-cache

# flashinfer ships ~6.5 GB of trtllm-gen cubins covering ONLY Blackwell datacenter parts
# (sm100a, sm103a, sm107a, sm100f). Not one of them serves sm_89 (RTX 40), sm_120 (RTX 50),
# sm_80 (A100) or sm_90a (H100) -- those come from flashinfer_jit_cache, whose 906 .so files
# are fat binaries carrying all six architectures. Dropping the cubins therefore costs the
# TensorRT-LLM FMHA and batched-GEMM fast paths on B200-class hardware and nothing else, and
# even there flashinfer JITs rather than failing.
#
# Pruned here, in the builder, and not in the final stage: a delete in a later layer leaves
# the bytes in an earlier one and shrinks the pull by nothing.
#
# Set --build-arg FLASHINFER_CUBINS=keep if you start renting B200s.
ARG FLASHINFER_CUBINS=prune
RUN set -eu; \
    cubins="$VENV/lib/python3.12/site-packages/flashinfer_cubin/cubins"; \
    if [ "$FLASHINFER_CUBINS" = prune ] && [ -d "$cubins" ]; then \
      before="$(du -sh "$cubins" | cut -f1)"; \
      find "$cubins" -mindepth 1 -delete; \
      echo "pruned flashinfer cubins: $before -> $(du -sh "$cubins" | cut -f1) (Blackwell datacenter kernels)"; \
    else \
      echo "keeping flashinfer cubins ($(du -sh "$cubins" | cut -f1))"; \
    fi

RUN "$VENV/bin/ft" --version && "$VENV/bin/hf" version


# --- final -------------------------------------------------------------------------
FROM ${VASTAI_IMAGE}

# No ENTRYPOINT here. The base image owns it: its entrypoint bootstraps boot_default.sh,
# which walks /etc/vast_boot.d and launches supervisor, which brings up Caddy, the Instance
# Portal and this image's freetoken-setup program. That happens in every Vast launch mode --
# the boot scripts detect the mode from /.launch and adapt -- so setup hooks in as a
# supervisor program (see docker/supervisor/) rather than fighting for PID 1.
#
# Neither this image nor the base ships an sshd; Vast injects one, but only in the launch
# modes that offer SSH. Entrypoint mode therefore leaves no way in but the browser terminal.
# docker/entrypoint.sh is kept for running this image outside Vast:
# `--entrypoint /opt/ft/entrypoint.sh`.
#
# nvcc and g++ are already present -- this base is cudnn-devel -- so the JIT fallback needs
# nothing installed. Verified: a real FreeToken kernel compiles on it.

COPY --from=freetoken-builder /opt/freetoken /opt/freetoken
COPY --from=ft-man-builder /build/target/release/ft-man /usr/local/bin/ft-man
COPY docker/entrypoint.sh docker/onstart.sh /opt/ft/
COPY docker/supervisor/freetoken-setup.conf /etc/supervisor/conf.d/freetoken-setup.conf
COPY docker/supervisor/freetoken-setup.sh /opt/supervisor-scripts/freetoken-setup.sh
RUN chmod 0755 /opt/ft/entrypoint.sh /opt/ft/onstart.sh /opt/supervisor-scripts/freetoken-setup.sh \
 && ln -sf /opt/freetoken/venv/bin/ft /usr/local/bin/ft \
 && ln -sf /opt/freetoken/venv/bin/hf /usr/local/bin/hf

# ft and hf are symlinked onto PATH rather than prepending the venv's bin directory, which
# would shadow the base image's own `python` and confuse its tooling. VIRTUAL_ENV is left
# alone for the same reason: ft-man's search list already contains /opt/freetoken/venv, so
# it resolves the engine with no environment help and no interference with /venv/main.
ENV HF_HOME=/workspace/hf \
    XDG_CACHE_HOME=/workspace/cache \
    FT_MAN_STATE_DIR=/workspace/state/ft-man \
    FT_MAN_CONFIG_DIR=/workspace/config/ft-man

# The engine itself is NOT exposed. It binds loopback inside the container so only Caddy can
# reach it, and Caddy publishes it on 18919 with the portal's TLS and authentication. Add
#     localhost:18919:1919:/:FreeToken API
# to the instance's PORTAL_CONFIG to make it appear in the Instance Portal. PORTAL_CONFIG is
# deliberately not set here: it comes from the template, and baking a value would silently
# drop the base image's own entries.
EXPOSE 18919

LABEL org.opencontainers.image.title="freetoken-ftman" \
      org.opencontainers.image.description="FreeToken MoE serving engine with the ft-man terminal UI, CUDA 13" \
      org.opencontainers.image.licenses="Apache-2.0"
