# Same image as relay-cloud/Dockerfile (Hostless hostless.yaml points here).
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
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates curl python3 gnupg \
  && curl -fsSL https://playit-cloud.github.io/ppa/key.gpg \
    | gpg --dearmor -o /usr/share/keyrings/playit.gpg \
  && echo "deb [signed-by=/usr/share/keyrings/playit.gpg] https://playit-cloud.github.io/ppa/data ./" \
    > /etc/apt/sources.list.d/playit-cloud.list \
  && apt-get update \
  && apt-get install -y --no-install-recommends playit \
  && rm -rf /var/lib/apt/lists/*
WORKDIR /relay
RUN mkdir -p /app/.cache
COPY relay-cloud/entrypoint.sh relay-cloud/health_server.py ./
COPY --from=tachyon-build /out/TachyonServer ./TachyonServer
RUN chmod +x TachyonServer entrypoint.sh
ENV TACHYON_KEY_DIR=/tmp/fluxrec/state
EXPOSE 8000
ENV PORT=8000
ENTRYPOINT ["./entrypoint.sh"]