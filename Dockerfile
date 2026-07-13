# Build from the repository root (the build needs the shared engine):
#
#   docker build -f cli_chat_standalone/Dockerfile -t atuin-ai-server .
#
# Run with the operator config mounted:
#
#   docker run -v ./config.toml:/etc/atuin-ai/config.toml -p 8080:8080 atuin-ai-server
#
# Debian rather than Alpine, matching the hub image (Alpine has DNS
# resolution issues in production).
ARG ELIXIR_VERSION=1.18.3
ARG OTP_VERSION=27.3.4.13
ARG DEBIAN_VERSION=bullseye-20260610-slim

ARG BUILDER_IMAGE="hexpm/elixir:${ELIXIR_VERSION}-erlang-${OTP_VERSION}-debian-${DEBIAN_VERSION}"
ARG RUNNER_IMAGE="debian:${DEBIAN_VERSION}"

FROM ${BUILDER_IMAGE} AS builder

# git and CA certs: gleam fetches the engine's dependencies, including a
# git dependency, during the build.
RUN apt-get update -y && apt-get install -y build-essential git curl ca-certificates \
    && apt-get clean && rm -f /var/lib/apt/lists/*_*

ARG GLEAM_VERSION=1.16.0
RUN case "$(uname -m)" in \
      x86_64) GLEAM_ARCH="x86_64" ;; \
      aarch64|arm64) GLEAM_ARCH="aarch64" ;; \
      *) echo "unsupported arch for gleam: $(uname -m)" && exit 1 ;; \
    esac && \
    curl -fsSL "https://github.com/gleam-lang/gleam/releases/download/v${GLEAM_VERSION}/gleam-v${GLEAM_VERSION}-${GLEAM_ARCH}-unknown-linux-musl.tar.gz" \
    | tar -xz -C /usr/local/bin gleam

RUN mix local.hex --force && \
    mix local.rebar --force

ENV MIX_ENV="prod"

WORKDIR /app/cli_chat_standalone

# Hex deps first, so they cache independently of source changes.
COPY cli_chat_standalone/mix.exs cli_chat_standalone/mix.lock ./
RUN mix deps.get --only $MIX_ENV
COPY cli_chat_standalone/config config
RUN mix deps.compile

# The shared engine changes more often than the hex deps; copy it late.
COPY gleam_cli_chat_core /app/gleam_cli_chat_core
COPY cli_chat_standalone/lib lib

RUN mix compile
RUN mix release

FROM ${RUNNER_IMAGE}

RUN apt-get update -y && \
    apt-get install -y libstdc++6 openssl libncurses5 locales ca-certificates \
    && apt-get clean && rm -f /var/lib/apt/lists/*_*

RUN sed -i '/en_US.UTF-8/s/^# //g' /etc/locale.gen && locale-gen

ENV LANG="en_US.UTF-8"
ENV LANGUAGE="en_US:en"
ENV LC_ALL="en_US.UTF-8"

WORKDIR /app
RUN chown nobody /app
USER nobody

COPY --from=builder --chown=nobody:root /app/cli_chat_standalone/_build/prod/rel/atuin_ai_server ./

ENV CHAT_CONFIG="/etc/atuin-ai/config.toml"
EXPOSE 8080

CMD ["/app/bin/atuin_ai_server", "start"]
