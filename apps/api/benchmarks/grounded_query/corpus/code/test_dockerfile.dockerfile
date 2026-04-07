# Multi-stage Dockerfile for a Node.js API application.
# Stage 1: Install dependencies
# Stage 2: Build TypeScript source
# Stage 3: Production image with minimal footprint

# ---------------------------------------------------------------------------
# Stage 1 - Dependency installation
# Uses a full Node image to install both production and dev dependencies.
# The node_modules output is reused by the build stage.
# ---------------------------------------------------------------------------
FROM node:20-bookworm-slim AS deps

WORKDIR /app

# Copy package manifests first for better layer caching.
# Changes to source code won't invalidate the dependency layer.
COPY package.json package-lock.json ./

RUN npm ci --ignore-scripts \
    && npm cache clean --force

# ---------------------------------------------------------------------------
# Stage 2 - Build
# Compiles TypeScript to JavaScript and runs any build-time code generation.
# Dev dependencies are available here but won't be in the final image.
# ---------------------------------------------------------------------------
FROM node:20-bookworm-slim AS build

WORKDIR /app

COPY --from=deps /app/node_modules ./node_modules
COPY package.json tsconfig.json ./
COPY src/ ./src/

# Run the TypeScript compiler and generate Prisma client
RUN npx prisma generate \
    && npx tsc --build tsconfig.json

# Remove dev dependencies after build to prepare a clean node_modules
RUN npm prune --production

# ---------------------------------------------------------------------------
# Stage 3 - Production runtime
# Uses a distroless-style slim image with only what's needed to run.
# No build tools, no dev dependencies, no source TypeScript files.
# ---------------------------------------------------------------------------
FROM node:20-bookworm-slim AS runtime

# Create a non-root user for security.
# The application should never run as root in production.
RUN groupadd --gid 1001 appuser \
    && useradd --uid 1001 --gid appuser --shell /bin/false appuser \
    && mkdir -p /app && chown appuser:appuser /app

WORKDIR /app

# Copy only production artifacts from the build stage
COPY --from=build --chown=appuser:appuser /app/node_modules ./node_modules
COPY --from=build --chown=appuser:appuser /app/dist ./dist
COPY --from=build --chown=appuser:appuser /app/package.json ./

# Copy runtime configuration files
COPY --chown=appuser:appuser prisma/ ./prisma/

# Environment configuration.
# DATABASE_URL and API_KEY must be provided at runtime.
ENV NODE_ENV=production \
    PORT=3000 \
    LOG_LEVEL=info

EXPOSE 3000

# Health check ensures the container is restarted if the API becomes
# unresponsive. Checks the /api/health endpoint every 30 seconds.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD node -e "fetch('http://localhost:3000/api/health').then(r => { if (!r.ok) process.exit(1) })"

USER appuser

# Use node directly instead of npm start to avoid an extra process layer
# and ensure signals (SIGTERM) are forwarded correctly.
CMD ["node", "dist/server.js"]
