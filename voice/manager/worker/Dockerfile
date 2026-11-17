# The manager's voice worker (LiveKit Agents, Python). Built by docker-compose.yml.
FROM python:3.13.18-slim-bookworm

ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    HOME=/app

WORKDIR /app
COPY requirements.txt .
RUN pip install -r requirements.txt

COPY worker.py providers.py wake.py ./
# Fetch whatever model files the plugins want now, not when the first person speaks.
RUN python worker.py download-files

RUN useradd --uid 10001 --home-dir /app --shell /usr/sbin/nologin worker && chown -R worker /app
USER worker

CMD ["python", "worker.py", "start"]
