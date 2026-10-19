#!/usr/bin/env node

const { randomBytes } = require('node:crypto');
const {
  closeSync,
  constants,
  fchmodSync,
  fsyncSync,
  openSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const version = process.env.IRONRAG_E2E_VERSION || 'v0.5.9';
if (!/^v\d+\.\d+\.\d+$/.test(version)) {
  throw new Error('IRONRAG_E2E_VERSION must be an immutable vX.Y.Z release tag');
}

const port = Number.parseInt(process.env.IRONRAG_E2E_PORT || '19500', 10);
if (!Number.isInteger(port) || port < 1024 || port > 65535) {
  throw new Error('IRONRAG_E2E_PORT must be an unprivileged TCP port');
}

const envPath = path.resolve(
  process.env.IRONRAG_E2E_ENV_FILE || path.join(os.tmpdir(), 'ironrag-youtrack-full-e2e.env'),
);
const apiToken = `irt_${randomBytes(32).toString('base64url')}`;
const baseUrl = `http://127.0.0.1:${port}`;
const values = {
  IRONRAG_BACKEND_IMAGE: `pipingspace/ironrag-backend:${version}`,
  IRONRAG_BACKEND_MEMORY_LIMIT: '768M',
  IRONRAG_CACHE_MEMORY_LIMIT: '128M',
  IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED: 'true',
  IRONRAG_CREDENTIAL_MASTER_KEY: randomBytes(32).toString('base64'),
  IRONRAG_CREDENTIAL_MASTER_KEY_ID: 'e2e-current',
  IRONRAG_DATABASE_MAX_CONNECTIONS: '32',
  IRONRAG_DB_MEMORY_LIMIT: '768M',
  IRONRAG_E2E_TOKEN: apiToken,
  IRONRAG_E2E_URL: baseUrl,
  IRONRAG_ENVIRONMENT: 'e2e',
  IRONRAG_FRONTEND_IMAGE: `pipingspace/ironrag-frontend:${version}`,
  IRONRAG_FRONTEND_MEMORY_LIMIT: '128M',
  IRONRAG_FRONTEND_ORIGIN: `${baseUrl},http://localhost:${port}`,
  IRONRAG_LOG_FILTER: 'warn',
  IRONRAG_OTEL_ENABLED: 'false',
  IRONRAG_PORT: `127.0.0.1:${port}`,
  IRONRAG_POSTGRES_DB: 'ironrag_e2e',
  IRONRAG_POSTGRES_PASSWORD: randomBytes(24).toString('hex'),
  IRONRAG_POSTGRES_USER: 'ironrag_e2e',
  IRONRAG_UI_BOOTSTRAP_ADMIN_API_TOKEN: apiToken,
  IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN: 'e2e-admin',
  IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD: `E2e_Aa1_${randomBytes(12).toString('hex')}`,
  IRONRAG_WORKER_MEMORY_LIMIT: '1536M',
  IRONRAG_WORKER_REPLICAS: '1',
  OTEL_LOGS_EXPORTER: 'none',
  OTEL_METRICS_EXPORTER: 'none',
  OTEL_TRACES_EXPORTER: 'none',
};
const contents = `${Object.entries(values)
  .map(([key, value]) => `${key}=${value}`)
  .join('\n')}\n`;

const temporaryPath = `${envPath}.${process.pid}.${randomBytes(4).toString('hex')}.tmp`;
let file;
try {
  file = openSync(
    temporaryPath,
    constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0),
    0o600,
  );
  writeFileSync(file, contents, 'utf8');
  fchmodSync(file, 0o600);
  fsyncSync(file);
  closeSync(file);
  file = undefined;
  renameSync(temporaryPath, envPath);
} finally {
  if (file !== undefined) closeSync(file);
  try {
    unlinkSync(temporaryPath);
  } catch (error) {
    if (error?.code !== 'ENOENT') throw error;
  }
}

console.log(`IronRAG E2E environment written to ${envPath} (mode 0600).`);
