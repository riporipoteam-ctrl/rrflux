FROM mcr.microsoft.com/dotnet/runtime-deps:8.0-jammy
ARG PLAYIT_TAG=v2.0.0-r1
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl python3 \
  && rm -rf /var/lib/apt/lists/* \
  && curl -fsSL -o /usr/local/bin/playitd \
    "https://github.com/playit-cloud/playit-agent/releases/download/${PLAYIT_TAG}/playitd-linux-x86_64" \
  && chmod +x /usr/local/bin/playitd
WORKDIR /relay
COPY TachyonServer entrypoint.sh health_server.py ./
RUN chmod +x TachyonServer entrypoint.sh
EXPOSE 7860
ENTRYPOINT ["./entrypoint.sh"]