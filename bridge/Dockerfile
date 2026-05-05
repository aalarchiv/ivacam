# Stage-1 FastAPI bridge container.
#
# This wraps the legacy viaConstructor Python core for early-stage testing
# while the Rust pipeline catches up. The server itself is throwaway — once
# wiac-server is feature-complete this image goes away.
#
# Build context expects to be the repo root so the image can copy both
# `bridge/` and `refs/viaconstructor/` (the bridge sys.path-imports it).
#
#   docker build -f bridge/Dockerfile -t wiac-bridge:dev .
#   docker run --rm -p 8765:8765 wiac-bridge:dev

FROM python:3.12-slim AS runtime

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    WIAC_HOST=0.0.0.0 \
    WIAC_PORT=8765

# viaConstructor pulls in matplotlib/Pillow/freetype which need a few system
# libs at runtime. Slim base is minimal so install what's actually needed.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libfreetype6 \
        libjpeg62-turbo \
        libpng16-16 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Install Python deps first so source edits don't bust the dep layer.
COPY bridge/requirements.txt /app/bridge/requirements.txt
RUN pip install --no-cache-dir -r /app/bridge/requirements.txt

# Bring the bridge package + the viaConstructor reference tree in.
COPY bridge/ /app/bridge/
COPY refs/viaconstructor/ /app/refs/viaconstructor/

ENV PYTHONPATH=/app/bridge:/app/refs/viaconstructor

EXPOSE 8765

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8765/healthz', timeout=2)"

CMD ["python", "-m", "wiac_bridge"]
