FROM python:3.12-slim-bookworm

ENV HOST=0.0.0.0 \
    PORT=8080 \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1

WORKDIR /app
COPY server.py /app/server.py

USER 65532:65532
EXPOSE 8080

HEALTHCHECK --interval=5s --timeout=2s --start-period=2s --retries=5 \
  CMD ["python", "-c", "import os, urllib.request; urllib.request.urlopen(f'http://127.0.0.1:{os.environ.get(\"PORT\", \"8080\")}/health', timeout=2).read()"]

CMD ["python", "/app/server.py"]
