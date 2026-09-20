# Local OpenAI HTTP simulator. Dummy credentials only. Not a provider proxy.
# Reuses the same Debian Bookworm runtime digest as the gateway image.
FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251

RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /app

COPY --chown=65534:65534 --chmod=0644 scripts/openai-simulator.py /app/openai-simulator.py

USER 65534:65534
WORKDIR /app

ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1

EXPOSE 38081

CMD ["python3", "/app/openai-simulator.py", "--host", "0.0.0.0", "--port", "38081", "--credential", "local-stack-dummy-only"]
