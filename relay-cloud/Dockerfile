# Flux Rec UDP relay — build context is repo root (Hostless: set root directory or use root Dockerfile).
FROM mcr.microsoft.com/dotnet/sdk:8.0 AS tachyon-build
WORKDIR /src
COPY tachyon-server/src/TachyonServer/TachyonServer.csproj tachyon-server/src/TachyonServer/
COPY tachyon-server/src/TachyonWire/TachyonWire.csproj tachyon-server/src/TachyonWire/
RUN dotnet restore tachyon-server/src/TachyonServer/TachyonServer.csproj
COPY tachyon-server/src/ tachyon-server/src/
RUN dotnet publish -c Release -r linux-x64 --self-contained true \
    -p:PublishSingleFile=true -p:IncludeNativeLibrariesForSelfExtract=true \
    tachyon-server/src/TachyonServer/TachyonServer.csproj \
    -o /out

FROM mcr.microsoft.com/dotnet/runtime-deps:8.0-jammy
ARG PLAYIT_TAG=v2.0.0-r1
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl python3 \
  && rm -rf /var/lib/apt/lists/* \
  && curl -fsSL -o /usr/local/bin/playitd \
    "https://github.com/playit-cloud/playit-agent/releases/download/${PLAYIT_TAG}/playitd-linux-x86_64" \
  && chmod +x /usr/local/bin/playitd
WORKDIR /relay
COPY relay-cloud/entrypoint.sh relay-cloud/health_server.py ./
COPY --from=tachyon-build /out/TachyonServer ./TachyonServer
RUN chmod +x TachyonServer entrypoint.sh
EXPOSE 8000
ENV PORT=8000
ENTRYPOINT ["./entrypoint.sh"]